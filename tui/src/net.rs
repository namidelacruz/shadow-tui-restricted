//! sakura net — network manager TUI (NetworkManager via nmcli). Self-repair, campus profiles
//! and device fixes go through `shadow wifi …`, which owns the self-correction logic.
//!
//!   ↑↓ move · Enter connect / activate · d disconnect · f forget · r rescan · w Wi-Fi on/off
//!   F fix device · c campus Wi-Fi (UCalgary / eduroam) · R repair connection · q quit
use crate::common::{active_theme, cmd_out, read_json, settings_path, Ui};
use crate::json::Value;
use crate::term::{Input, Key, Style, Term};

fn nm(args: &[&str]) -> (i32, String) {
    let bin = std::env::var("SAKURA_NMCLI").unwrap_or_else(|_| "nmcli".into());
    let mut v = vec![bin.as_str()];
    v.extend_from_slice(args);
    cmd_out(&v)
}

/// nmcli -t splits on unescaped ':'
pub fn split_t(line: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut esc = false;
    for c in line.chars() {
        if esc {
            out.last_mut().unwrap().push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == ':' {
            out.push(String::new());
        } else {
            out.last_mut().unwrap().push(c);
        }
    }
    out
}

#[derive(Clone, Debug)]
enum Row {
    Header(String),
    Device { dev: String, kind: String, state: String, conn: String },
    Wifi { inuse: bool, signal: u32, security: String, ssid: String },
    Saved { name: String, kind: String, active: bool },
}

enum Prompt {
    Psk(String),
    CampusUser(String),
    CampusPass(String, String),
}

struct App {
    rows: Vec<Row>,
    sel: usize,
    scroll: usize,
    radio: bool,
    health: String,
    msg: (String, bool),
    prompt: Option<(Input, Prompt)>,
    ui: Ui,
    quit: bool,
}

impl App {
    fn refresh(&mut self, rescan: bool) {
        let mut rows = vec![Row::Header("DEVICES".into())];
        for l in nm(&["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"]).1.lines() {
            let f = split_t(l);
            if f.len() >= 4 && f[1] != "loopback" && !f[1].starts_with("wifi-p2p") {
                rows.push(Row::Device { dev: f[0].clone(), kind: f[1].clone(), state: f[2].clone(), conn: f[3].clone() });
            }
        }
        self.radio = nm(&["radio", "wifi"]).1.trim() == "enabled";
        rows.push(Row::Header(format!("WI-FI NETWORKS{}", if self.radio { "" } else { " (Wi-Fi is off — w to turn on)" })));
        if self.radio {
            let mut seen = std::collections::HashSet::new();
            let args: Vec<&str> = if rescan {
                vec!["-t", "-f", "IN-USE,SIGNAL,SECURITY,SSID", "device", "wifi", "list", "--rescan", "yes"]
            } else {
                vec!["-t", "-f", "IN-USE,SIGNAL,SECURITY,SSID", "device", "wifi", "list"]
            };
            let mut nets: Vec<Row> = vec![];
            for l in nm(&args).1.lines() {
                let f = split_t(l);
                if f.len() < 4 || f[3].is_empty() || !seen.insert(f[3].clone()) {
                    continue;
                }
                nets.push(Row::Wifi { inuse: f[0] == "*", signal: f[1].parse().unwrap_or(0), security: f[2].clone(), ssid: f[3].clone() });
            }
            nets.sort_by_key(|r| if let Row::Wifi { inuse, signal, .. } = r { (!inuse, std::cmp::Reverse(*signal)) } else { (true, std::cmp::Reverse(0)) });
            rows.extend(nets);
        }
        rows.push(Row::Header("SAVED CONNECTIONS".into()));
        for l in nm(&["-t", "-f", "NAME,TYPE,ACTIVE", "connection", "show"]).1.lines() {
            let f = split_t(l);
            if f.len() >= 3 && f[1] != "loopback" {
                rows.push(Row::Saved { name: f[0].clone(), kind: f[1].replace("802-11-wireless", "wifi").replace("802-3-ethernet", "ethernet"), active: f[2] == "yes" });
            }
        }
        self.rows = rows;
        let (_, h) = cmd_out(&["shadow", "wifi", "status"]);
        self.health = h.lines().find_map(|l| l.strip_prefix("health")).map(|s| s.trim_start_matches(|c| c == ' ' || c == ':').to_string()).unwrap_or_default();
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
        if matches!(self.rows.get(self.sel), Some(Row::Header(_))) {
            self.step(1);
        }
    }

    fn step(&mut self, d: i32) {
        let n = self.rows.len() as i32;
        let mut i = self.sel as i32;
        for _ in 0..n {
            i = (i + d).clamp(0, n - 1);
            if !matches!(self.rows[i as usize], Row::Header(_)) {
                self.sel = i as usize;
                return;
            }
            if i == 0 || i == n - 1 {
                return;
            }
        }
    }

    fn draw(&mut self, t: &mut Term) {
        let u = &self.ui;
        let (w, h) = (t.w, t.h);
        t.clear(u.base);
        t.fill(0, 0, w, 1, u.sel);
        t.text(0, 0, " ❀ dot//sakura ▸ NETWORK", u.sel, w);
        let health = format!("{} ", if self.health.is_empty() { "-".into() } else { self.health.clone() });
        t.text(w.saturating_sub(health.chars().count() + 1), 0, &health, u.sel, w / 2);
        let body = h.saturating_sub(4);
        if self.sel < self.scroll {
            self.scroll = self.sel;
        }
        if self.sel >= self.scroll + body {
            self.scroll = self.sel + 1 - body;
        }
        for (i, r) in self.rows.iter().enumerate().skip(self.scroll).take(body) {
            let y = 2 + i - self.scroll;
            let selected = i == self.sel;
            if selected {
                t.fill(1, y, w - 2, 1, u.sel);
            }
            let st = |s: Style| if selected { u.sel } else { s };
            match r {
                Row::Header(s) => {
                    t.text(1, y, s, u.accent, w);
                }
                Row::Device { dev, kind, state, conn } => {
                    let bad = state.starts_with("unavailable") || state.starts_with("unmanaged");
                    t.text(3, y, &format!("{dev:<12} {kind:<10} {state:<16} {conn}"), st(if bad { u.alert } else { u.base }), w - 4);
                }
                Row::Wifi { inuse, signal, security, ssid } => {
                    let bars = ["▂   ", "▂▄  ", "▂▄▆ ", "▂▄▆█"][((*signal as usize) / 26).min(3)];
                    let lock = if security.is_empty() || security == "--" { "open" } else if security.contains("802.1X") { "enterprise" } else { "secured" };
                    t.text(3, y, &format!("{} {bars} {signal:>3}%  {lock:<10} {ssid}", if *inuse { "●" } else { " " }), st(if *inuse { u.ok } else { u.base }), w - 4);
                }
                Row::Saved { name, kind, active } => {
                    t.text(3, y, &format!("{} {kind:<10} {name}", if *active { "●" } else { " " }), st(if *active { u.ok } else { u.dim }), w - 4);
                }
            }
        }
        t.fill(0, h - 2, w, 1, u.panel);
        t.text(1, h - 2, &self.msg.0, if self.msg.1 { Style { bg: u.theme.c("bg_alt"), ..u.alert } } else { u.panel }, w - 2);
        if let Some((inp, p)) = &self.prompt {
            let (label, mask) = match p {
                Prompt::Psk(s) => (format!("password for {s} ▸ "), Some('•')),
                Prompt::CampusUser(_) => ("username ▸ ".into(), None),
                Prompt::CampusPass(..) => ("password ▸ ".into(), Some('•')),
            };
            t.text(0, h - 1, &label, u.accent, w);
            inp.draw(t, label.chars().count(), h - 1, w.saturating_sub(label.chars().count() + 1), u.input, true, mask);
        } else {
            t.text(0, h - 1, " enter connect · d disconnect · f forget · r rescan · w wifi on/off · F fix device · c campus · R repair · q quit", u.dim, w);
        }
    }
}

/// Run a shadownet/nmcli command with the screen handed over (shows the repair log live).
fn live(t: Term, cmd: &[&str], stdin: Option<String>) -> Option<Term> {
    let mut t = t;
    t.leave();
    drop(t);
    println!("\x1b[1m── {} ──\x1b[0m", cmd.join(" "));
    let mut c = std::process::Command::new(cmd[0]);
    c.args(&cmd[1..]);
    if stdin.is_some() {
        c.stdin(std::process::Stdio::piped());
    }
    let code = match c.spawn() {
        Ok(mut ch) => {
            if let (Some(d), Some(mut si)) = (stdin, ch.stdin.take()) {
                let _ = std::io::Write::write_all(&mut si, d.as_bytes());
            }
            ch.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1)
        }
        Err(_) => 127,
    };
    print!("\n[exit {code}] Enter to return… ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let mut l = String::new();
    let _ = std::io::stdin().read_line(&mut l);
    Term::enter(None).ok()
}

pub fn run(_args: &[String]) -> i32 {
    let settings = read_json(&settings_path()).unwrap_or(Value::Obj(vec![]));
    let Ok(mut t) = Term::enter(None) else {
        eprintln!("sakura net: needs a terminal");
        return 1;
    };
    t.mouse(true);
    let mut a = App { rows: vec![], sel: 0, scroll: 0, radio: true, health: String::new(), msg: ("r rescans · F fixes \"device not ready\" / \"device down\"".into(), false), prompt: None, ui: Ui::new(&active_theme(&settings), false), quit: false };
    a.refresh(false);
    let mut term = Some(t);
    while !a.quit {
        let mut t = term.take().unwrap();
        if t.resize() {
            t.force_redraw();
        }
        a.draw(&mut t);
        let _ = t.present();
        let mut cur = Some(t);
        for k in cur.as_mut().unwrap().keys() {
            let tt = cur.take().unwrap();
            cur = key(&mut a, tt, k);
            if cur.is_none() || a.quit {
                break;
            }
        }
        let Some(mut t) = cur else { return 1 };
        t.mouse(true);
        term = Some(t);
    }
    if let Some(mut t) = term {
        t.leave();
    }
    0
}

fn key(a: &mut App, t: Term, k: Key) -> Option<Term> {
    if let Some((mut inp, p)) = a.prompt.take() {
        match k {
            Key::Esc => a.msg = ("cancelled".into(), false),
            Key::Enter => {
                let v = inp.buf.clone();
                match p {
                    Prompt::Psk(ssid) => {
                        let r = live(t, &["shadow", "wifi", "connect", &ssid, "--password-stdin"], Some(format!("{v}\n")));
                        a.refresh(false);
                        return r;
                    }
                    Prompt::CampusUser(preset) => {
                        if v.trim().is_empty() {
                            a.msg = ("username can't be empty".into(), true);
                            a.prompt = Some((inp, Prompt::CampusUser(preset)));
                        } else {
                            a.prompt = Some((Input::default(), Prompt::CampusPass(preset, v.trim().to_string())));
                        }
                    }
                    Prompt::CampusPass(preset, user) => {
                        let r = live(t, &["shadow", "wifi", "campus", &preset, "--user", &user, "--password-stdin"], Some(format!("{v}\n")));
                        a.refresh(false);
                        return r;
                    }
                }
            }
            _ => {
                inp.handle(k);
                a.prompt = Some((inp, p));
            }
        }
        return Some(t);
    }
    let row = a.rows.get(a.sel).cloned();
    match k {
        Key::Char('q') | Key::Esc | Key::Ctrl('c') => a.quit = true,
        Key::Up | Key::Char('k') => a.step(-1),
        Key::Down | Key::Char('j') => a.step(1),
        Key::Char('r') => {
            a.msg = ("rescanning…".into(), false);
            a.refresh(true);
            a.msg = ("scan complete".into(), false);
        }
        Key::Char('w') => {
            let (code, out) = nm(&["radio", "wifi", if a.radio { "off" } else { "on" }]);
            a.msg = (if code == 0 { format!("Wi-Fi {}", if a.radio { "off" } else { "on" }) } else { out }, code != 0);
            a.refresh(false);
        }
        Key::Char('F') => {
            let r = live(t, &["shadow", "wifi", "fix-device"], None);
            a.refresh(false);
            return r;
        }
        Key::Char('R') => {
            let r = live(t, &["shadow", "wifi", "repair"], None);
            a.refresh(false);
            return r;
        }
        Key::Char('c') => {
            a.msg = ("campus Wi-Fi: UCalgary username = first half of your email (eduroam: run `shadow wifi campus eduroam`)".into(), false);
            a.prompt = Some((Input::default(), Prompt::CampusUser("ucalgary".into())));
        }
        Key::Char('d') => match row {
            Some(Row::Device { dev, .. }) => { let (c, o) = nm(&["device", "disconnect", &dev]); a.msg = (if c == 0 { format!("{dev} disconnected") } else { o }, c != 0); a.refresh(false) }
            Some(Row::Saved { name, .. }) => { let (c, o) = nm(&["connection", "down", "id", &name]); a.msg = (if c == 0 { format!("{name} down") } else { o }, c != 0); a.refresh(false) }
            _ => {}
        },
        Key::Char('f') => if let Some(Row::Saved { name, .. }) = row {
            let (c, o) = nm(&["connection", "delete", "id", &name]);
            a.msg = (if c == 0 { format!("forgot {name}") } else { o }, c != 0);
            a.refresh(false);
        },
        Key::Enter | Key::Char(' ') => match row {
            Some(Row::Wifi { ssid, security, inuse, .. }) => {
                if inuse {
                    a.msg = (format!("already connected to {ssid}"), false);
                } else if security.contains("802.1X") {
                    a.msg = ("enterprise network: press c for the UCalgary / eduroam setup (self-correcting)".into(), false);
                } else if security.is_empty() || security == "--" {
                    let r = live(t, &["shadow", "wifi", "connect", &ssid, "--password-stdin"], Some("\n".into()));
                    a.refresh(false);
                    return r;
                } else {
                    a.prompt = Some((Input::default(), Prompt::Psk(ssid)));
                }
            }
            Some(Row::Saved { name, active, .. }) => {
                if !active {
                    let r = live(t, &["shadow", "wifi", "repair", &name], None);
                    a.refresh(false);
                    return r;
                }
            }
            Some(Row::Device { dev, .. }) => { let (c, o) = nm(&["device", "connect", &dev]); a.msg = (if c == 0 { format!("{dev} connecting") } else { o }, c != 0); a.refresh(false) }
            _ => {}
        },
        Key::Mouse(64, ..) => a.step(-1),
        Key::Mouse(65, ..) => a.step(1),
        Key::Mouse(0, _, y, true) => {
            let i = a.scroll + y.saturating_sub(2);
            if y >= 2 && i < a.rows.len() && !matches!(a.rows[i], Row::Header(_)) {
                a.sel = i;
            }
        }
        _ => {}
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    #[test]
    fn split_escaped() {
        assert_eq!(super::split_t(r"*:80:WPA2:My\:Net"), vec!["*", "80", "WPA2", "My:Net"]);
    }
}
