//! shadow-prefs: default apps and the crash log.
//!
//! Usage:
//!
//!   shadow-prefs          DEFAULTS: file manager, terminal, background
//!   shadow-prefs crash    CRASH LOG: what crashed, and what is holding changes back
//!   shadow-prefs report   write the crash report file and print its path (works on a text console)
//!
//! DEFAULTS:
//! - **File manager:** sets the XDG default for folders (`xdg-mime … inode/directory`) and
//!   what Mod+E starts.
//! - **Terminal:** sets what Mod+Return starts, what terminal apps from the hub, launcher and
//!   files open in, and `~/.config/xdg-terminals.list`.
//! - **Background:** Vertices (the 3D field), Aurora, or an image.
//! - **Solid background colour:** on or off.
//!
//! Each change applies at once. niri reloads its config live.
//!
//! CRASH LOG gathers, newest first:
//! - **Things holding changes back:** an interrupted update, niri refusing the new config,
//!   the hub's fallback, changed defaults waiting as .shadow-new.
//! - **Crashes:** core dumps of Shadow programs and niri (coredumpctl), hub exits, Rust panics
//!   in the session log, and errors in the journal.
//!
//! Select a line to see its details below. "Save a report" writes everything to one file in
//! your home folder, to read or send.
//!
//! Keys: Tab switches page · ↑↓ select · ← → change a default · Enter apply / open ·
//! PgUp/PgDn scroll the details · r reload · Esc close
use shadow_apps::cfg::{get_in, set_in};
use shadow_apps::desktop;
use shadow_apps::{atomic_write, cmd_out, config_path, home, sh_quote, spawn, wall, TERMINALS};
use shadow_ui::widgets::{chrome, footer, panel, selection, Rect};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};
use std::path::{Path, PathBuf};

const PAGES: [&str; 2] = ["DEFAULTS", "CRASH LOG"];

#[derive(Clone, PartialEq)]
enum Act {
    Files,
    Terminal,
    Background,
    Solid,
    /// open in a terminal (kept open)
    Term(String),
    /// start a program
    Exec(String),
    Report,
    Clear,
    None,
}

#[derive(Clone)]
struct Row {
    lvl: char, // '✗' bad · '!' warning · '✓' fine · '·' info
    text: String,
    value: String,
    detail: String,
    body: Vec<String>,
    act: Act,
    /// newest first ordering on the crash page
    when: String,
}

fn row(lvl: char, text: impl Into<String>, detail: impl Into<String>, act: Act) -> Row {
    Row { lvl, text: text.into(), value: String::new(), detail: detail.into(), body: vec![], act, when: String::new() }
}

fn conf() -> String {
    std::fs::read_to_string(config_path()).unwrap_or_default()
}

fn conf_get(section: &str, key: &str) -> Option<String> {
    get_in(&conf(), section, key).map(|v| v.trim_matches('"').trim().to_string()).filter(|v| !v.is_empty())
}

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn state_dir() -> PathBuf {
    // an empty XDG_STATE_HOME counts as unset (as in the session script's ${XDG_STATE_HOME:-…})
    std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".local/state")).join("shadow-tui")
}

fn have(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

// ── defaults ─────────────────────────────────────────────────────────────────────────────────────

/// (desktop id, name, command) of every app that opens folders, Shadow's own first
fn file_managers() -> Vec<(String, String, String)> {
    let mut v = vec![("shadow-files.desktop".to_string(), "Shadow files".to_string(), "shadow-files".to_string())];
    let mut seen = std::collections::HashSet::new();
    seen.insert("shadow-files.desktop".to_string());
    for d in desktop::data_dirs() {
        let Ok(rd) = std::fs::read_dir(d.join("applications")) else { continue };
        let mut files: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "desktop")).collect();
        files.sort();
        for p in files {
            let id = p.file_name().unwrap().to_string_lossy().to_string();
            if seen.contains(&id) {
                continue;
            }
            let Ok(t) = std::fs::read_to_string(&p) else { continue };
            let main = t.split("\n[").next().unwrap_or("");
            let get = |k: &str| main.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(str::trim);
            if get("NoDisplay") == Some("true") || get("Hidden") == Some("true") {
                continue;
            }
            if !get("MimeType").unwrap_or("").split(';').any(|m| m == "inode/directory") {
                continue;
            }
            let (Some(name), Some(exec)) = (get("Name"), get("Exec")) else { continue };
            seen.insert(id.clone());
            v.push((id, name.to_string(), desktop::strip_field_codes(exec)));
        }
    }
    v
}

/// (command, name) of the terminals installed here
fn terminals() -> Vec<(String, String)> {
    TERMINALS.iter().filter(|t| t.0 == "shadow-term" || have(t.0)).map(|t| (t.0.to_string(), t.1.to_string())).collect()
}

/// the desktop id a terminal command ships, for xdg-terminals.list
fn terminal_desktop_id(cmd: &str) -> Option<String> {
    for d in desktop::data_dirs() {
        let Ok(rd) = std::fs::read_dir(d.join("applications")) else { continue };
        for e in rd.flatten() {
            let Ok(t) = std::fs::read_to_string(e.path()) else { continue };
            let main = t.split("\n[").next().unwrap_or("");
            let exec = main.lines().find_map(|l| l.strip_prefix("Exec=")).unwrap_or("");
            let first = exec.split_whitespace().next().unwrap_or("");
            if Path::new(first).file_name().is_some_and(|f| f == cmd) && main.contains("TerminalEmulator") {
                return Some(e.file_name().to_string_lossy().to_string());
            }
        }
    }
    None
}

const BACKGROUNDS: [(&str, &str); 2] = [("vertices", "Vertices (3D, follows the cursor)"), ("default", "Aurora (generated image)")];

// ── crash log ────────────────────────────────────────────────────────────────────────────────────

fn tail(p: &Path, n: usize) -> Vec<String> {
    let t = std::fs::read_to_string(p).unwrap_or_default();
    let v: Vec<String> = t.lines().map(String::from).collect();
    v[v.len().saturating_sub(n)..].to_vec()
}

/// leading ISO timestamp of a log line ("2026-11-02T05:13:42-06:00 …"), if any
fn stamp(l: &str) -> String {
    let w = l.split_whitespace().next().unwrap_or("");
    if w.len() >= 19 && w.as_bytes()[4] == b'-' && w.as_bytes()[10] == b'T' {
        w[..19].replace('T', " ")
    } else {
        String::new()
    }
}

/// Everything known about crashes and stuck changes, worst first, then newest first.
fn crash_rows() -> Vec<Row> {
    let mut held: Vec<Row> = vec![];
    let mut crashes: Vec<Row> = vec![];
    let sd = state_dir();
    let session = sd.join("session.log");
    let hublog = sd.join("hub.log");
    let slines = tail(&session, 2000);

    // ── what is holding changes back ──
    let mark = Path::new("/var/lib/shadow-tui/update-in-progress");
    if mark.exists() {
        let mut r = row('✗', "An update was interrupted — new logins are refused until it is finished", "Enter: finish or undo it now (asks for your password); a reboot also does it", Act::Term("sudo shadow update recover".into()));
        r.body = std::fs::read_to_string(mark).unwrap_or_default().lines().map(String::from).collect();
        held.push(r);
    }
    if let Some(i) = slines.iter().rposition(|l| l.contains("niri rejected the generated config")) {
        let later_ok = slines[i..].iter().any(|l| l.starts_with("wrote ") || l.contains("using the config"));
        if !later_ok {
            let mut r = row('✗', "niri refused the new config — your latest settings are not applied", "Enter: try again and show niri's reason", Act::Term("shadow setup niri".into()));
            r.body = slines[i.saturating_sub(2)..(i + 3).min(slines.len())].to_vec();
            held.push(r);
        }
    }
    if let Some(l) = slines.iter().rev().find(|l| l.contains("shadow setup niri: using the config")) {
        let mut r = row('·', "niri uses a reduced Shadow config", l.trim_start_matches("shadow setup niri: ").to_string(), Act::None);
        r.body = vec![l.clone(), "An older niri doesn't know some optional lines; everything else works. Updating niri brings them back.".into()];
        held.push(r);
    }
    let hl = tail(&hublog, 400);
    if let Some(l) = hl.iter().rev().find(|l| l.contains("falling back")) {
        let since = hl.iter().rposition(|x| x == l).unwrap_or(0);
        let mut r = row('✗', "The hub stopped and the side bar took over", "Enter: start the hub again (shadow-hub switch)", Act::Term("shadow-hub switch".into()));
        r.when = stamp(l);
        r.body = hl[since.saturating_sub(6)..=since].to_vec();
        r.body.push("Its own output just before is in ~/.local/state/shadow-tui/hub.log.".into());
        held.push(r);
    }
    for dir in ["/etc/shadow-tui", "/etc/pam.d"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if let Some(base) = n.strip_suffix(".shadow-new") {
                    held.push(row('!', format!("A changed default is waiting: {dir}/{base}"), format!("compare with: diff {dir}/{base} {dir}/{n}"), Act::Term(format!("diff -u {dir}/{base} {dir}/{n}; echo; echo 'to take the new one: sudo mv {dir}/{n} {dir}/{base}'"))));
                }
            }
        }
    }

    // ── crashes ──
    // core dumps of Shadow programs and niri
    let (c, o) = cmd_out(&["coredumpctl", "list", "--no-pager", "--no-legend", "-q", "--since=-14d"]);
    if c == 0 {
        for l in o.lines() {
            // "Mon 2026-11-02 05:13:42 MST  12345  1000  1000  SIGSEGV  present  /opt/shadow-tui/bin/shadow-hub  1.2M"
            let w: Vec<&str> = l.split_whitespace().collect();
            let Some(exe) = w.iter().find(|x| x.starts_with('/')) else { continue };
            let prog = Path::new(exe).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            if !(prog.starts_with("shadow") || prog == "niri") {
                continue;
            }
            let sig = w.iter().find(|x| x.starts_with("SIG")).copied().unwrap_or("?");
            let pid = w.get(4).copied().unwrap_or("");
            let when = format!("{} {}", w.get(1).copied().unwrap_or(""), w.get(2).copied().unwrap_or(""));
            let mut r = row('✗', format!("{prog} crashed ({sig})"), format!("{when} · Enter: full report (coredumpctl info {pid})"), Act::Term(format!("coredumpctl info {pid} --no-pager | less")));
            r.when = when;
            r.body = vec![l.trim().to_string()];
            crashes.push(r);
        }
    }
    // hub exits (the supervisor restarted it)
    for l in hl.iter().filter(|l| l.contains("the hub exited")) {
        let mut r = row('!', "The hub exited and was restarted", l.splitn(2, ' ').nth(1).unwrap_or("").to_string(), Act::None);
        r.when = stamp(l);
        r.body = vec![l.clone()];
        crashes.push(r);
    }
    // Rust panics of Shadow programs (their stderr goes to the session log; the hub's to hub.log)
    let mut panic_src: Vec<String> = slines.clone();
    panic_src.extend(hl.iter().cloned());
    let slines_all = panic_src;
    let mut seen_panics = std::collections::HashSet::new();
    for (i, l) in slines_all.iter().enumerate() {
        let slines = &slines_all;
        if l.contains("panicked at") || l.contains("panicked:") {
            // "panicked at apps/src/bin/net.rs:212:9" → shadow-net; else a "shadow-…" word in the line
            let lead = l.split_whitespace().next().filter(|w| w.starts_with("shadow-")).map(|w| w.trim_end_matches(':').to_string());
            let prog = lead.or_else(|| l
                .split("src/bin/")
                .nth(1)
                .and_then(|r| r.split(".rs").next())
                .map(|b| format!("shadow-{b}"))
                .or_else(|| l.split(['\'', ' ', ':']).find(|w| w.starts_with("shadow")).map(String::from)))
                .unwrap_or_else(|| "a Shadow program".into());
            // the same panic repeated (a restart loop) is one entry
            let key = l.split("panicked").nth(1).unwrap_or(l).trim().to_string();
            if !seen_panics.insert(format!("{prog}{key}")) {
                continue;
            }
            let mut r = row('✗', format!("{prog} panicked"), l.trim().chars().take(140).collect::<String>(), Act::None);
            r.body = slines[i.saturating_sub(3)..(i + 6).min(slines.len())].to_vec();
            r.when = slines[..i].iter().rev().map(|x| stamp(x)).find(|s| !s.is_empty()).unwrap_or_default();
            crashes.push(r);
        }
    }
    // niri itself ending abnormally (shadow-session logs it)
    for l in slines.iter().filter(|l| l.contains("niri exited")) {
        let mut r = row('✗', "niri stopped unexpectedly", l.splitn(2, ' ').nth(1).unwrap_or("").to_string(), Act::None);
        r.when = stamp(l);
        r.body = vec![l.clone()];
        crashes.push(r);
    }
    // errors in the journal: your session's Shadow programs and the system services
    for (scope, args) in [("session", vec!["journalctl", "--user", "-p", "err", "-n", "40", "--no-pager", "-o", "short-iso", "-q"]), ("system", vec!["journalctl", "-u", "shadow-*", "-p", "err", "-n", "40", "--no-pager", "-o", "short-iso", "-q"])] {
        let (c, o) = cmd_out(&args);
        if c != 0 {
            continue;
        }
        for l in o.lines().filter(|l| l.contains("shadow") || l.contains("niri")) {
            let msg = l.splitn(4, ' ').nth(3).unwrap_or(l).to_string();
            let mut r = row('!', format!("{scope} error: {}", msg.chars().take(90).collect::<String>()), l.split_whitespace().next().unwrap_or("").to_string(), Act::None);
            r.when = stamp(l);
            r.body = vec![l.to_string()];
            crashes.push(r);
        }
    }
    crashes.sort_by(|a, b| b.when.cmp(&a.when));
    crashes.truncate(60);

    // problems first (what holds changes back, then crashes, newest first), the tools after them
    let mut v = vec![];
    if held.is_empty() {
        v.push(row('✓', "Nothing is holding changes back", "no interrupted update, niri accepts the config, the hub runs, no waiting defaults", Act::None));
    }
    v.extend(held);
    if crashes.is_empty() {
        v.push(row('✓', "No crashes recorded", "core dumps (14 days), hub exits, panics and journal errors are all clear", Act::None));
    }
    v.extend(crashes);
    v.extend([
        row('·', "Save a report", "one plain-text file in ~/shadow-reports (personal details replaced) — attach or upload it yourself", Act::Report),
        row('·', "Open the reports folder", "~/shadow-reports", Act::Exec("xdg-open ~/shadow-reports".into())),
        row('·', "Check the hub", "shadow-hub doctor", Act::Term("shadow-hub doctor".into())),
        row('·', "Open the fix center", "app problems, tasks, login, updates, desktop", Act::Exec("shadow-fix".into())),
        row('·', "Clear the Shadow logs", "empties session.log and hub.log (press twice)", Act::Clear),
    ]);
    v
}

/// Where saved reports go: one plain-text file each, for you to attach or upload by hand
fn reports_dir() -> PathBuf {
    home().join("shadow-reports")
}

/// Replace personal details in a report: your user name, home folder and computer name, Wi-Fi
/// network names, MAC and IP addresses, e-mail addresses, and anything shaped like a password or
/// token. What stays is what's needed to fix a crash.
pub fn redact(text: &str, user: &str, hostname: &str, ssids: &[String]) -> String {
    let mut t = text.to_string();
    let home_dir = home().to_string_lossy().to_string();
    if home_dir.len() > 1 {
        t = t.replace(&home_dir, "~");
    }
    for (needle, with) in [(user, "<user>"), (hostname, "<host>")] {
        if needle.len() >= 2 {
            t = t.replace(needle, with);
        }
    }
    for s in ssids.iter().filter(|s| s.len() >= 2) {
        t = t.replace(s.as_str(), "<wifi>");
    }
    let mut out = String::with_capacity(t.len());
    for line in t.lines() {
        let low = line.to_lowercase();
        // whole value after a secret-looking key
        let secret_key = ["password", "passwd", "psk", "token", "secret", "api_key", "apikey", "authorization", "private_key"].iter().find(|k| low.contains(*k));
        if let Some(k) = secret_key {
            if let Some(pos) = low.find(*k) {
                let rest = &line[pos + k.len()..];
                if let Some(sep) = rest.find([':', '=']) {
                    out.push_str(&line[..pos + k.len() + sep + 1]);
                    out.push_str(" <redacted>\n");
                    continue;
                }
            }
        }
        let words: Vec<String> = line
            .split_inclusive([' ', '\t', ',', ';', '(', ')', '[', ']', '"', '\''])
            .map(|w| {
                let core = w.trim_end_matches([' ', '\t', ',', ';', '(', ')', '[', ']', '"', '\'']);
                let tail = &w[core.len()..];
                let red = if is_mac(core) {
                    "<mac>".to_string()
                } else if is_ipv4(core) {
                    "<ip>".to_string()
                } else if core.contains('@') && core.contains('.') && !core.starts_with('@') && core.len() > 5 {
                    "<email>".to_string()
                } else if core.starts_with("ghp_") || core.starts_with("github_pat_") || core.starts_with("gho_") || core.starts_with("sk-") || (core.len() >= 32 && core.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') && core.chars().any(|c| c.is_ascii_digit()) && core.chars().any(|c| c.is_ascii_uppercase()) && core.chars().any(|c| c.is_ascii_lowercase())) {
                    "<token>".to_string()
                } else {
                    core.to_string()
                };
                red + tail
            })
            .collect();
        out.push_str(&words.concat());
        out.push('\n');
    }
    out
}

fn is_mac(s: &str) -> bool {
    let p: Vec<&str> = s.split(':').collect();
    p.len() == 6 && p.iter().all(|x| x.len() == 2 && x.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_ipv4(s: &str) -> bool {
    let s = s.split('/').next().unwrap_or(s);
    let p: Vec<&str> = s.split('.').collect();
    p.len() == 4 && p.iter().all(|x| !x.is_empty() && x.len() <= 3 && x.parse::<u16>().is_ok_and(|v| v <= 255)) && !s.starts_with("0.") && s != "127.0.0.1"
}

fn write_report(rows: &[Row]) -> Result<PathBuf, String> {
    let date = cmd_out(&["date", "+%Y%m%d-%H%M%S"]).1.trim().to_string();
    let dir = reports_dir();
    let p = dir.join(format!("shadow-report-{date}.txt"));
    let mut t = String::new();
    t.push_str(&format!("Shadow TUI Restricted crash report — {}\n", cmd_out(&["date", "-Is"]).1.trim()));
    t.push_str("Personal details (user and computer name, Wi-Fi names, MAC / IP / e-mail addresses, passwords and tokens) are replaced by <placeholders>.\nRead it before you send it: it is plain text.\n\n");
    for (label, args) in [("shadow", vec!["shadow", "--version"]), ("compositor", vec!["shadow-compositor", "--version"]), ("kernel", vec!["uname", "-r"]), ("os", vec!["sh", "-c", ". /etc/os-release && echo \"$PRETTY_NAME\""])] {
        t.push_str(&format!("{label}: {}\n", cmd_out(&args).1.lines().next().unwrap_or("?").trim()));
    }
    t.push_str(&format!("hardware: {}\n", shadow_apps::hw::profile().summary()));
    t.push_str("\n== hub check (shadow-hub doctor) ==\n");
    t.push_str(&cmd_out(&["shadow-hub", "doctor"]).1);
    t.push_str("\n== crash log ==\n");
    for r in rows.iter().filter(|r| !matches!(r.act, Act::Report | Act::Clear | Act::Exec(_)) && r.text != "Check the hub") {
        t.push_str(&format!("[{}] {} — {}\n", r.lvl, r.text, r.detail));
        for b in &r.body {
            t.push_str(&format!("      {b}\n"));
        }
    }
    let sd = state_dir();
    for (name, n) in [("session.log", 300), ("hub.log", 120)] {
        t.push_str(&format!("\n== {name} (last {n} lines) ==\n"));
        for l in tail(&sd.join(name), n) {
            t.push_str(&l);
            t.push('\n');
        }
    }
    let user = std::env::var("USER").unwrap_or_default();
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default().trim().to_string();
    let ssids: Vec<String> = cmd_out(&["nmcli", "-t", "-f", "NAME", "connection", "show"]).1.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && !l.starts_with("lo")).collect();
    let mut t = redact(&t, &user, &host, &ssids);
    // keep it attachable everywhere: at most 1 MB, the newest part kept
    if t.len() > 1_000_000 {
        let cut = t.len() - 1_000_000;
        let cut = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(cut);
        t = format!("[… earlier lines cut to keep the report under 1 MB]\n{}", &t[cut..]);
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    atomic_write(&p, &t).map_err(|e| e.to_string())?;
    Ok(p)
}

// ── the app ──────────────────────────────────────────────────────────────────────────────────────

struct Prefs {
    page: usize,
    rows: Vec<Row>,
    sel: usize,
    top: usize,
    visible: usize,
    msg: (String, bool),
    hits: Vec<(usize, Rect)>,
    tabs: Vec<(usize, Rect)>,
    body_top: usize,
    armed: bool,
}

impl Prefs {
    fn load(&mut self) {
        self.rows = if PAGES[self.page] == "DEFAULTS" { self.defaults() } else { crash_rows() };
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
        self.body_top = 0;
    }

    fn defaults(&self) -> Vec<Row> {
        let fm = file_managers();
        let cur_fm = conf_get("defaults", "files").or_else(|| {
            let (c, o) = cmd_out(&["xdg-mime", "query", "default", "inode/directory"]);
            (c == 0).then(|| o.trim().to_string()).filter(|s| !s.is_empty())
        });
        let fm_name = cur_fm.as_ref().and_then(|id| fm.iter().find(|f| &f.0 == id).map(|f| f.1.clone())).or(cur_fm.clone()).unwrap_or_else(|| "Shadow files".into());
        let term = shadow_apps::terminal();
        let term_name = TERMINALS.iter().find(|t| t.0 == term.split_whitespace().next().unwrap_or("")).map(|t| t.1.to_string()).unwrap_or(term.clone());
        let (bg, _) = wall::choice(&conf(), None);
        let bg_name = BACKGROUNDS.iter().find(|b| b.0 == bg).map(|b| b.1.to_string()).unwrap_or_else(|| format!("Image: {}", Path::new(&bg).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(bg.clone())));
        let solid = conf_get("look", "solid_background").map(|v| v != "false").unwrap_or(true);
        let mut v = vec![
            Row { value: fm_name, ..row('·', "File manager", format!("{} installed · folders open in it, and Mod+E", fm.len()), Act::Files) },
            Row { value: term_name, ..row('·', "Terminal", format!("{} installed · Mod+Return, and terminal apps from the hub, launcher and files", terminals().len()), Act::Terminal) },
            Row { value: bg_name, ..row('·', "Background", "Vertices · Aurora · an image (Enter on \"Choose an image\" below)", Act::Background) },
            Row { value: if solid { "on".into() } else { "off".into() }, ..row('·', "Solid background colour", "the flat obsidian behind the desktop and the overview", Act::Solid) },
            row('·', "Choose an image…", "the wallpaper picker: browse, per monitor, fill / fit", Act::Exec("shadow-wallpaper pick".into())),
        ];
        for r in v.iter_mut() {
            r.lvl = '·';
        }
        v
    }

    /// change the default on the selected row by `dir` (+1 / -1)
    fn cycle(&mut self, dir: i32) {
        let Some(r) = self.rows.get(self.sel).cloned() else { return };
        let step = |n: usize, cur: Option<usize>| -> usize { ((cur.map(|c| c as i32).unwrap_or(-1) + dir).rem_euclid(n.max(1) as i32)) as usize };
        let mut t = conf();
        let result: Result<String, String> = match r.act {
            Act::Files => {
                let fm = file_managers();
                let cur = conf_get("defaults", "files").and_then(|id| fm.iter().position(|f| f.0 == id));
                let (id, name, cmd) = fm[step(fm.len(), cur)].clone();
                t = set_in(&t, "defaults", "files", &q(&id));
                t = set_in(&t, "defaults", "files_cmd", &q(&cmd));
                let (c, o) = cmd_out(&["xdg-mime", "default", &id, "inode/directory"]);
                if c != 0 && c != 127 {
                    Err(format!("xdg-mime: {}", o.trim()))
                } else {
                    Ok(format!("file manager: {name}"))
                }
            }
            Act::Terminal => {
                let ts = terminals();
                let cur_cmd = shadow_apps::terminal();
                let cur = ts.iter().position(|x| x.0 == cur_cmd);
                let (cmd, name) = ts[step(ts.len(), cur)].clone();
                t = set_in(&t, "defaults", "terminal_cmd", &q(&cmd));
                // the cross-desktop default terminal list (xdg-terminal-exec), when the terminal ships a desktop entry
                if let Some(id) = terminal_desktop_id(&cmd) {
                    let p = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("xdg-terminals.list");
                    let _ = atomic_write(&p, &format!("{id}\n"));
                }
                Ok(format!("terminal: {name}"))
            }
            Act::Background => {
                let (cur, _) = wall::choice(&t, None);
                let i = BACKGROUNDS.iter().position(|b| b.0 == cur);
                let (path, name) = BACKGROUNDS[step(BACKGROUNDS.len(), i)];
                self.msg = match wall::set(path, None, None) {
                    Ok(()) => (format!("background: {name} — applied"), false),
                    Err(e) => (e, true),
                };
                let keep = self.sel;
                self.load();
                self.sel = keep;
                return;
            }
            Act::Solid => {
                let on = conf_get("look", "solid_background").map(|v| v != "false").unwrap_or(true);
                t = set_in(&t, "look", "solid_background", if on { "false" } else { "true" });
                Ok(format!("solid background colour {}", if on { "off" } else { "on" }))
            }
            _ => return,
        };
        let r = result.and_then(|m| atomic_write(&config_path(), &t).map(|_| m).map_err(|e| e.to_string()));
        match r {
            Ok(m) => {
                // Mod+Return / Mod+E / the background colour live in niri's config
                let (c, o) = cmd_out(&["shadow", "setup", "compositor"]);
                let extra = if c == 0 { " — applied".to_string() } else { format!(" — saved; niri: {}", o.lines().last().unwrap_or("").trim()) };
                self.msg = (format!("{m}{extra}"), c != 0);
            }
            Err(e) => self.msg = (e, true),
        }
        let keep = self.sel;
        self.load();
        self.sel = keep;
    }

    fn activate(&mut self, ctx: &mut Ctx) {
        let Some(r) = self.rows.get(self.sel).cloned() else { return };
        match r.act {
            Act::Files | Act::Terminal | Act::Background | Act::Solid => self.cycle(1),
            Act::Term(cmd) => {
                spawn(&format!("shadow-term --hold -e sh -c {}", sh_quote(&cmd)));
                self.msg = (format!("opened a terminal: {cmd}"), false);
            }
            Act::Exec(cmd) => {
                if cmd.contains("shadow-reports") {
                    let _ = std::fs::create_dir_all(reports_dir());
                }
                spawn(&cmd)
            }
            Act::Report => {
                self.msg = match write_report(&self.rows) {
                    Ok(p) => {
                        let copied = std::process::Command::new("wl-copy").arg(p.to_string_lossy().to_string()).stdin(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
                        (format!("saved {}{}", p.display(), if copied { " (path copied)" } else { "" }), false)
                    }
                    Err(e) => (format!("could not save the report: {e}"), true),
                }
            }
            Act::Clear => {
                if self.armed {
                    self.armed = false;
                    let sd = state_dir();
                    for f in ["session.log", "hub.log"] {
                        let _ = std::fs::write(sd.join(f), "");
                    }
                    self.load();
                    self.msg = ("logs cleared".into(), false);
                } else {
                    self.armed = true;
                    self.msg = ("press Enter again to clear session.log and hub.log".into(), false);
                }
            }
            Act::None => return,
        }
        ctx.redraw();
    }
}

impl App for Prefs {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-prefs".into(), title: "Preferences".into(), size: (1000, 680) }
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        let n = self.rows.len();
        if k.sym != Keysym::Return && k.sym != Keysym::KP_Enter {
            self.armed = false;
        }
        match k.sym {
            Keysym::Escape | Keysym::q => ctx.quit(),
            Keysym::Tab | Keysym::ISO_Left_Tab => {
                self.page = (self.page + 1) % PAGES.len();
                self.sel = 0;
                self.top = 0;
                self.msg.0.clear();
                self.load();
            }
            Keysym::Up => {
                self.sel = self.sel.saturating_sub(1);
                self.body_top = 0;
            }
            Keysym::Down => {
                self.sel = (self.sel + 1).min(n.saturating_sub(1));
                self.body_top = 0;
            }
            Keysym::Left => self.cycle(-1),
            Keysym::Right => self.cycle(1),
            Keysym::Page_Down => self.body_top += 5,
            Keysym::Page_Up => self.body_top = self.body_top.saturating_sub(5),
            Keysym::r => {
                self.load();
                self.msg = ("reloaded".into(), false);
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => self.activate(ctx),
            _ => return,
        }
        ctx.redraw();
    }

    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        if let Some(&(p, _)) = self.tabs.iter().find(|(_, r)| r.hit(x, y)) {
            self.page = p;
            self.sel = 0;
            self.top = 0;
            self.msg.0.clear();
            self.load();
            ctx.redraw();
        } else if let Some(&(i, _)) = self.hits.iter().find(|(_, r)| r.hit(x, y)) {
            if self.sel == i {
                self.activate(ctx);
            } else {
                self.sel = i;
                self.body_top = 0;
                ctx.redraw();
            }
        }
    }

    fn scroll(&mut self, lines: i32, ctx: &mut Ctx) {
        let n = self.rows.len() as i32;
        self.sel = (self.sel as i32 + lines).clamp(0, (n - 1).max(0)) as usize;
        self.body_top = 0;
        ctx.redraw();
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let crash = PAGES[self.page] == "CRASH LOG";
        let bad = self.rows.iter().filter(|r| r.lvl == '✗').count();
        let status = if crash { if bad == 0 { "all clear".to_string() } else { format!("{bad} to look at") } } else { "applies at once".into() };
        let (top, pad) = chrome(f, "PREFERENCES", &status);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt;
        let spt = pt * 0.84;
        let lh = f.line_h(pt);
        let slh = f.line_h(spt);
        self.tabs.clear();
        let mut x = pad;
        for (i, p) in PAGES.iter().enumerate() {
            let r = shadow_ui::widgets::button(f, x, top, p, i == self.page, true);
            self.tabs.push((i, r));
            x += r.w + f.px(8.0);
        }
        let list_top = top + lh + f.px(26.0);
        let row_h = lh + slh + f.px(12.0);
        let foot = f.line_h(pt * 0.88) + pad + f.px(14.0) + lh + f.px(6.0);
        // the details panel (crash log): the selected line's full text
        let body_h = if crash { (h / 4).max(row_h * 2) } else { 0 };
        self.visible = ((h - list_top - foot - body_h) / row_h).max(1) as usize;
        if self.sel < self.top {
            self.top = self.sel;
        }
        if self.sel >= self.top + self.visible {
            self.top = self.sel + 1 - self.visible;
        }
        self.hits.clear();
        let rows = self.rows.clone();
        for (i, r) in rows.iter().enumerate().skip(self.top).take(self.visible) {
            let y = list_top + (i - self.top) as i32 * row_h;
            let rect = Rect { x: pad, y, w: w - 2 * pad, h: row_h - f.px(4.0) };
            if i == self.sel {
                selection(f, rect.x, rect.y, rect.w, rect.h);
            }
            let col = match r.lvl {
                '!' => th.amber,
                '✗' => th.crimson,
                '✓' => th.green,
                _ => th.cyan,
            };
            let mx = pad + f.px(16.0);
            f.text(mx, y + f.px(4.0), &r.lvl.to_string(), pt, col, true);
            let tx = mx + f.px(22.0);
            // value of a default: "◂ Foot ▸" on the right
            let mut right = w - pad - f.px(10.0);
            if !r.value.is_empty() {
                let v = format!("◂ {} ▸", r.value);
                let v = f.fit(&v, pt, true, (w - 2 * pad) / 2);
                let vw = f.text_w(&v, pt, true);
                f.text(right - vw, y + f.px(4.0), &v, pt, if i == self.sel { th.amber } else { th.cyan }, true);
                right -= vw + f.px(16.0);
            }
            let t = f.fit(&r.text, pt, i == self.sel, right - tx);
            f.text(tx, y + f.px(4.0), &t, pt, if r.lvl == '✓' { th.dim } else { th.text }, i == self.sel);
            let d = f.fit(&r.detail, spt, false, w - pad - tx - f.px(10.0));
            f.text(tx, y + f.px(4.0) + lh, &d, spt, th.dim, false);
            self.hits.push((i, rect));
        }
        if crash {
            let by = h - foot - body_h + f.px(4.0);
            panel(f, pad, by, w - 2 * pad, body_h - f.px(8.0), th.cyan);
            let raw: Vec<String> = match self.rows.get(self.sel) {
                Some(r) if !r.body.is_empty() => r.body.clone(),
                Some(r) => vec![r.text.clone(), r.detail.clone()],
                None => vec![],
            };
            // wrap to the panel width (by characters: the font is monospaced)
            let cw = f.text_w("0000000000", spt, false).max(1) as f32 / 10.0;
            let cols = (((w - 4 * pad) as f32 / cw) as usize).max(20);
            let mut lines: Vec<String> = vec![];
            for l in raw {
                let chars: Vec<char> = l.chars().collect();
                if chars.is_empty() {
                    lines.push(String::new());
                }
                for chunk in chars.chunks(cols) {
                    lines.push(chunk.iter().collect());
                }
            }
            let max_lines = ((body_h - f.px(28.0)) / slh).max(1) as usize;
            self.body_top = self.body_top.min(lines.len().saturating_sub(1));
            for (k, l) in lines.iter().skip(self.body_top).take(max_lines).enumerate() {
                let s = f.fit(l, spt, false, w - 4 * pad);
                f.text(pad + f.px(14.0), by + f.px(10.0) + k as i32 * slh, &s, spt, th.text, false);
            }
            if lines.len() > max_lines {
                let more = format!("{}–{} of {} · PgUp/PgDn", self.body_top + 1, (self.body_top + max_lines).min(lines.len()), lines.len());
                let mw = f.text_w(&more, spt * 0.9, false);
                f.text(w - pad - mw - f.px(14.0), by + body_h - f.px(8.0) - slh - f.px(6.0), &more, spt * 0.9, th.dim, false);
            }
        }
        if !self.msg.0.is_empty() {
            let my = h - foot + f.px(2.0);
            let m = f.fit(&self.msg.0, pt, false, w - 2 * pad);
            f.text(pad + f.px(8.0), my, &m, pt, if self.msg.1 { th.crimson } else { th.green }, false);
        }
        footer(f, if crash { "Tab page · ↑↓ select · Enter open · PgUp/PgDn details · r reload · Esc close" } else { "Tab page · ↑↓ select · ← → or Enter change · applies at once · Esc close" });
    }
}

fn main() {
    // on a text console too: write the report and print where it is
    if std::env::args().nth(1).as_deref() == Some("report") {
        match write_report(&crash_rows()) {
            Ok(p) => {
                println!("{}", p.display());
                std::process::exit(0)
            }
            Err(e) => {
                eprintln!("shadow-prefs: {e}");
                std::process::exit(1)
            }
        }
    }
    let page = match std::env::args().nth(1).as_deref() {
        Some("crash") | Some("crashes") | Some("log") => 1,
        _ => 0,
    };
    let mut p = Prefs { page, rows: vec![], sel: 0, top: 0, visible: 8, msg: (String::new(), false), hits: vec![], tabs: vec![], body_top: 0, armed: false };
    p.load();
    if let Err(e) = run(Box::new(p), None) {
        eprintln!("shadow-prefs: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_hide_personal_details() {
        let t = "user nami on nami-laptop at /home/nami/x\nwlan0 ssid=UC-Secure bssid 3c:7c:3f:aa:bb:cc ip 192.168.1.42/24\npsk=hunter2hunter2\nmail nami@example.com token ghp_ABCdef1234567890ABCdef1234567890ab\npanicked at apps/src/bin/net.rs:10:5 (0.9.2)\n";
        let r = redact(t, "nami", "nami-laptop", &["UC-Secure".to_string()]);
        for gone in ["nami", "UC-Secure", "3c:7c:3f", "192.168", "hunter2", "example.com", "ghp_"] {
            assert!(!r.contains(gone), "{gone} still in: {r}");
        }
        assert!(r.contains("panicked at apps/src/bin/net.rs:10:5 (0.9.2)"), "the useful part stays: {r}");
        assert!(r.contains("<mac>") && r.contains("<ip>") && r.contains("<wifi>") && r.contains("<redacted>"));
    }

    #[test]
    fn timestamps_from_log_lines() {
        assert_eq!(stamp("2026-11-02T05:13:42-06:00 the hub exited"), "2026-11-02 05:13:42");
        assert_eq!(stamp("wrote /x"), "");
    }

    #[test]
    fn crash_page_reads_the_logs() {
        let d = std::env::temp_dir().join(format!("prefs-{}", std::process::id()));
        let sd = d.join("shadow-tui");
        std::fs::create_dir_all(&sd).unwrap();
        std::env::set_var("XDG_STATE_HOME", &d);
        std::fs::write(sd.join("hub.log"), "shadow-hub panicked: panicked at apps/src/vertex.rs:205:16:\ncopy_from_slice: source slice length (16400644) does not match destination slice length (16400704)\n2026-11-02T05:13:42-06:00 the hub exited (signal: 11 (SIGSEGV)) after 6 s\nshadow-hub panicked: panicked at apps/src/vertex.rs:205:16:\n2026-11-02T05:13:44-06:00 falling back to the side bar and the wallpaper: it stopped 3 times\n").unwrap();
        std::fs::write(sd.join("session.log"), "shadow setup niri: niri rejected the generated config — kept the current one\nunexpected node `foo`\nthread 'main' panicked at apps/src/bin/net.rs:10:5:\nboom\n").unwrap();
        let rows = crash_rows();
        let has = |t: &str| rows.iter().any(|r| r.text.contains(t));
        assert!(has("side bar took over"), "fallback shown");
        assert!(has("niri refused the new config"), "rejected config shown");
        assert!(has("exited and was restarted"), "hub exit shown");
        assert!(rows.iter().any(|r| r.text == "shadow-net panicked"), "the panicking program is named");
        // the hub's own panic, from hub.log, once even though it repeated
        assert_eq!(rows.iter().filter(|r| r.text == "shadow-hub panicked").count(), 1);
        // what holds changes back comes before the crashes
        let held = rows.iter().position(|r| r.text.contains("niri refused")).unwrap();
        let crash = rows.iter().position(|r| r.text.contains("exited and was restarted")).unwrap();
        assert!(held < crash);
        let _ = std::fs::remove_dir_all(&d);
    }
}
