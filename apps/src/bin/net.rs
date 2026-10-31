//! shadow-net — Wi-Fi management (2D app on the shared UI toolkit).
//!
//! Left: devices, networks in range, saved connections. Right: the link's health and the live
//! log of whatever is running — connecting, the UCalgary / eduroam 802.1X setup and the
//! self-repair loop print their EAP/TLS phases there as they happen.
//!
//!   ↑↓/jk select · Enter connect · c campus setup · R repair · F fix device · i details
//!   d disconnect · f forget · r rescan · w Wi-Fi on/off · Esc/q close
use shadow_apps::cmd_out;
use shadow_ui::calloop::channel::{self, Sender};
use shadow_ui::widgets::{chrome, footer, Input};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[derive(Clone)]
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

struct Net {
    tx: Sender<String>,
    rows: Vec<Row>,
    sel: usize,
    top: usize,
    radio: bool,
    status: Vec<String>,
    log: Vec<String>,
    busy: Option<String>,
    msg: (String, bool),
    prompt: Option<(Input, Prompt)>,
}

/// nmcli -t splits on unescaped ':'
fn split_t(line: &str) -> Vec<String> {
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

fn nm(args: &[&str]) -> (i32, String) {
    let mut a = vec!["nmcli"];
    a.extend_from_slice(args);
    cmd_out(&a)
}

impl Net {
    fn refresh(&mut self) {
        let mut rows = vec![Row::Header("DEVICES".into())];
        for l in nm(&["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"]).1.lines() {
            let f = split_t(l);
            if f.len() >= 4 && f[1] != "loopback" && !f[1].starts_with("wifi-p2p") {
                rows.push(Row::Device { dev: f[0].clone(), kind: f[1].clone(), state: f[2].clone(), conn: f[3].clone() });
            }
        }
        self.radio = nm(&["radio", "wifi"]).1.trim() == "enabled";
        rows.push(Row::Header(if self.radio { "IN RANGE".into() } else { "IN RANGE · Wi-Fi off (w)".into() }));
        if self.radio {
            let mut seen = std::collections::HashSet::new();
            let mut nets = vec![];
            for l in nm(&["-t", "-f", "IN-USE,SIGNAL,SECURITY,SSID", "device", "wifi", "list", "--rescan", "no"]).1.lines() {
                let f = split_t(l);
                if f.len() < 4 || f[3].is_empty() || !seen.insert(f[3].clone()) {
                    continue;
                }
                nets.push(Row::Wifi { inuse: f[0] == "*", signal: f[1].parse().unwrap_or(0), security: f[2].clone(), ssid: f[3].clone() });
            }
            nets.sort_by_key(|r| if let Row::Wifi { inuse, signal, .. } = r { (!inuse, std::cmp::Reverse(*signal)) } else { (true, std::cmp::Reverse(0)) });
            rows.extend(nets);
        }
        rows.push(Row::Header("SAVED".into()));
        for l in nm(&["-t", "-f", "NAME,TYPE,ACTIVE", "connection", "show"]).1.lines() {
            let f = split_t(l);
            if f.len() >= 3 && f[1] != "loopback" {
                rows.push(Row::Saved { name: f[0].clone(), kind: f[1].replace("802-11-wireless", "wifi").replace("802-3-ethernet", "ethernet"), active: f[2] == "yes" });
            }
        }
        self.rows = rows;
        self.status = cmd_out(&["shadow", "wifi", "status"]).1.lines().map(String::from).filter(|l| !l.trim().is_empty()).take(24).collect();
        if self.status.is_empty() {
            self.status = vec!["shadow wifi status: not available".into()];
        }
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

    /// Run a command on a worker thread, streaming its output into the log pane.
    fn live(&mut self, title: &str, cmd: Vec<String>, stdin: Option<String>) {
        if self.busy.is_some() {
            self.msg = ("wait for the running task to finish".into(), true);
            return;
        }
        self.busy = Some(title.to_string());
        self.log = vec![format!("▸ {}", cmd.join(" ").replace("--password-stdin", "(password via stdin)"))];
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let child = Command::new(&cmd[0]).args(&cmd[1..]).stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
            let mut c = match child {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(format!("log:{}: {e}", cmd[0]));
                    let _ = tx.send("done:127".into());
                    return;
                }
            };
            if let (Some(s), Some(mut i)) = (stdin, c.stdin.take()) {
                let _ = i.write_all(s.as_bytes());
            }
            if let Some(err) = c.stderr.take() {
                let tx2 = tx.clone();
                std::thread::spawn(move || {
                    for l in BufReader::new(err).lines().map_while(Result::ok) {
                        let _ = tx2.send(format!("log:{l}"));
                    }
                });
            }
            if let Some(out) = c.stdout.take() {
                for l in BufReader::new(out).lines().map_while(Result::ok) {
                    let _ = tx.send(format!("log:{l}"));
                }
            }
            let code = c.wait().ok().and_then(|s| s.code()).unwrap_or(1);
            let _ = tx.send(format!("done:{code}"));
        });
    }

    fn sh(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    fn submit_prompt(&mut self, inp: Input, p: Prompt) {
        let v = inp.text.clone();
        match p {
            Prompt::Psk(ssid) => self.live(&format!("connect {ssid}"), Self::sh(&["shadow", "wifi", "connect", &ssid, "--password-stdin"]), Some(format!("{v}\n"))),
            Prompt::CampusUser(preset) => {
                if v.trim().is_empty() {
                    self.msg = ("username can't be empty".into(), true);
                    self.prompt = Some((inp, Prompt::CampusUser(preset)));
                } else {
                    self.prompt = Some((Input::secret(), Prompt::CampusPass(preset, v.trim().to_string())));
                }
            }
            Prompt::CampusPass(preset, user) => self.live(&format!("campus {preset}"), Self::sh(&["shadow", "wifi", "campus", &preset, "--user", &user, "--password-stdin"]), Some(format!("{v}\n"))),
        }
    }
}

impl App for Net {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-net".into(), title: "Network".into(), size: (1000, 640) }
    }

    fn message(&mut self, m: String, ctx: &mut Ctx) {
        if let Some(l) = m.strip_prefix("log:") {
            self.log.push(l.chars().filter(|c| !c.is_control()).collect());
            if self.log.len() > 400 {
                self.log.drain(..100);
            }
        } else if let Some(c) = m.strip_prefix("done:") {
            let ok = c == "0";
            self.msg = (format!("{} — {}", self.busy.take().unwrap_or_default(), if ok { "done" } else { "failed (see log)" }), !ok);
            self.refresh();
        } else if m == "rescanned" {
            self.busy = None;
            self.refresh();
            self.msg = ("scan complete".into(), false);
        }
        ctx.redraw();
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        ctx.redraw();
        if let Some((mut inp, p)) = self.prompt.take() {
            match k.sym {
                Keysym::Escape => self.msg = ("cancelled".into(), false),
                Keysym::Return | Keysym::KP_Enter => self.submit_prompt(inp, p),
                _ => {
                    inp.key(k);
                    self.prompt = Some((inp, p));
                }
            }
            return;
        }
        let row = self.rows.get(self.sel).cloned();
        let typed = k.typed().unwrap_or("").to_string();
        match (k.sym, typed.as_str()) {
            (Keysym::Escape, _) | (_, "q") => ctx.quit(),
            (Keysym::Up, _) | (_, "k") => self.step(-1),
            (Keysym::Down, _) | (_, "j") => self.step(1),
            (_, "r") => {
                if self.busy.is_none() {
                    self.busy = Some("rescan".into());
                    self.msg = ("rescanning…".into(), false);
                    let tx = self.tx.clone();
                    std::thread::spawn(move || {
                        let _ = nm(&["device", "wifi", "rescan"]);
                        std::thread::sleep(std::time::Duration::from_secs(3));
                        let _ = tx.send("rescanned".into());
                    });
                }
            }
            (_, "w") => {
                let (code, out) = nm(&["radio", "wifi", if self.radio { "off" } else { "on" }]);
                self.msg = (if code == 0 { format!("Wi-Fi {}", if self.radio { "off" } else { "on" }) } else { out }, code != 0);
                self.refresh();
            }
            (_, "F") => self.live("fix device", Self::sh(&["shadow", "wifi", "fix-device"]), None),
            (_, "R") => self.live("repair", Self::sh(&["shadow", "wifi", "repair"]), None),
            (_, "i") => self.live("details", Self::sh(&["shadow", "wifi", "status"]), None),
            (_, "c") => {
                self.msg = ("campus Wi-Fi (UC-Secure): username = the first half of your email".into(), false);
                self.prompt = Some((Input::default(), Prompt::CampusUser("ucalgary".into())));
            }
            (_, "e") => {
                self.msg = ("eduroam: username = your full institutional email".into(), false);
                self.prompt = Some((Input::default(), Prompt::CampusUser("eduroam".into())));
            }
            (_, "d") => match row {
                Some(Row::Device { dev, .. }) => {
                    let (c, o) = nm(&["device", "disconnect", &dev]);
                    self.msg = (if c == 0 { format!("{dev} disconnected") } else { o }, c != 0);
                    self.refresh();
                }
                Some(Row::Saved { name, .. }) => {
                    let (c, o) = nm(&["connection", "down", "id", &name]);
                    self.msg = (if c == 0 { format!("{name} down") } else { o }, c != 0);
                    self.refresh();
                }
                _ => {}
            },
            (_, "f") => {
                if let Some(Row::Saved { name, .. }) = row {
                    let (c, o) = nm(&["connection", "delete", "id", &name]);
                    self.msg = (if c == 0 { format!("forgot {name}") } else { o }, c != 0);
                    self.refresh();
                }
            }
            (Keysym::Return | Keysym::KP_Enter, _) | (_, " ") => match row {
                Some(Row::Wifi { ssid, security, inuse, .. }) => {
                    if inuse {
                        self.msg = (format!("already connected to {ssid}"), false);
                    } else if security.contains("802.1X") {
                        self.msg = ("enterprise network: c = UC-Secure setup, e = eduroam setup".into(), false);
                    } else if security.is_empty() || security == "--" {
                        self.live(&format!("connect {ssid}"), Self::sh(&["shadow", "wifi", "connect", &ssid, "--password-stdin"]), Some("\n".into()));
                    } else {
                        self.prompt = Some((Input::secret(), Prompt::Psk(ssid)));
                    }
                }
                Some(Row::Saved { name, active, .. }) if !active => self.live(&format!("repair {name}"), Self::sh(&["shadow", "wifi", "repair", &name]), None),
                Some(Row::Device { dev, .. }) => {
                    let (c, o) = nm(&["device", "connect", &dev]);
                    self.msg = (if c == 0 { format!("{dev} connecting") } else { o }, c != 0);
                    self.refresh();
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let health = self.status.iter().find_map(|l| l.trim().strip_prefix("health")).map(|s| s.trim_start_matches([' ', ':']).to_string()).unwrap_or_default();
        let st = match &self.busy {
            Some(b) => format!("● {b}…"),
            None => health,
        };
        let (top, pad) = chrome(f, "NETWORK", &st);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt;
        let spt = pt * 0.9;
        let lh = f.line_h(pt);
        let slh = f.line_h(spt);
        let foot_h = f.line_h(spt) + pad + f.px(4.0);
        let bottom = h - foot_h - lh * 2 - f.px(10.0);
        let split = pad + (w - 2 * pad) * 46 / 100;
        // list
        let row_h = lh + f.px(4.0);
        let visible = ((bottom - top) / row_h).max(1) as usize;
        if self.sel < self.top {
            self.top = self.sel;
        }
        if self.sel >= self.top + visible {
            self.top = self.sel + 1 - visible;
        }
        for (i, r) in self.rows.iter().enumerate().skip(self.top).take(visible) {
            let y = top + (i - self.top) as i32 * row_h;
            let sel = i == self.sel;
            if sel {
                f.cv.fill(pad, y, split - pad - f.px(10.0), row_h, th.amber, 0.14);
                f.cv.fill(pad, y, f.px(3.0), row_h, th.amber, 1.0);
            }
            let x = pad + f.px(10.0);
            let maxw = split - x - f.px(16.0);
            let (text, col) = match r {
                Row::Header(s) => (s.clone(), th.amber),
                Row::Device { dev, kind, state, conn } => {
                    let bad = state.starts_with("unavailable") || state.starts_with("unmanaged");
                    (format!("{dev}  {kind}  {state}  {conn}"), if bad { th.crimson } else { th.text })
                }
                Row::Wifi { inuse, signal, security, ssid } => {
                    let bars = ["▂   ", "▂▄  ", "▂▄▆ ", "▂▄▆█"][((*signal as usize) / 26).min(3)];
                    let lock = if security.is_empty() || security == "--" { "open" } else if security.contains("802.1X") { "802.1X" } else { "psk" };
                    (format!("{} {bars} {signal:>3}%  {lock:<6} {ssid}", if *inuse { "●" } else { " " }), if *inuse { th.green } else { th.text })
                }
                Row::Saved { name, kind, active } => (format!("{} {kind:<9} {name}", if *active { "●" } else { " " }), if *active { th.green } else { th.dim }),
            };
            let t = f.fit(&text, pt, false, maxw);
            f.text(x, y + f.px(2.0), &t, pt, col, matches!(r, Row::Header(_)));
        }
        // right pane: live log while something runs, otherwise the link status
        let hair = f.hair();
        f.cv.fill(split, top, hair, bottom - top, th.amber, 0.35);
        let rx = split + f.px(14.0);
        let rw = w - pad - rx;
        let (head, lines): (&str, &Vec<String>) = if self.busy.is_some() || self.log.len() > 1 { ("LIVE LOG", &self.log) } else { ("LINK", &self.status) };
        f.text(rx, top, head, spt, th.amber, true);
        let max_lines = ((bottom - top - slh - f.px(6.0)) / slh).max(1) as usize;
        let start = lines.len().saturating_sub(max_lines);
        for (n, l) in lines.iter().skip(start).enumerate() {
            let y = top + slh + f.px(6.0) + n as i32 * slh;
            let low = l.to_lowercase();
            let col = if low.contains("fail") || low.contains("error") || low.contains("denied") || low.contains("✗") {
                th.crimson
            } else if low.contains(" ok") || low.contains("✓") || low.contains("success") || low.contains("connected") {
                th.green
            } else if low.contains("eap") || low.contains("tls") || low.contains("cert") || low.contains("phase") {
                th.cyan
            } else {
                th.text
            };
            let t = f.fit(l, spt, false, rw);
            f.text(rx, y, &t, spt, col, false);
        }
        // message + prompt
        let my = bottom + f.px(6.0);
        if !self.msg.0.is_empty() {
            let m = f.fit(&self.msg.0, pt, false, w - 2 * pad);
            f.text(pad, my, &m, pt, if self.msg.1 { th.crimson } else { th.cyan }, false);
        }
        if let Some((inp, p)) = &self.prompt {
            let label = match p {
                Prompt::Psk(s) => format!("password for {s} ▸ "),
                Prompt::CampusUser(_) => "username ▸ ".into(),
                Prompt::CampusPass(..) => "password ▸ ".into(),
            };
            let py = my + lh + f.px(4.0);
            let lw = f.text(pad, py, &label, pt, th.amber, true);
            inp.draw(f, pad + lw, py, pt, true);
        }
        footer(f, "Enter connect · c UC-Secure · e eduroam · R repair · F fix device · i details · d disconnect · f forget · r rescan · w Wi-Fi · q close");
    }
}

fn main() {
    let (tx, rx) = channel::channel::<String>();
    let mut n = Net { tx, rows: vec![], sel: 0, top: 0, radio: false, status: vec![], log: vec![], busy: None, msg: (String::new(), false), prompt: None };
    n.refresh();
    if let Err(e) = run(Box::new(n), Some(rx)) {
        eprintln!("shadow-net: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn split_escaped() {
        assert_eq!(super::split_t(r"*:80:WPA2:My\:Net"), vec!["*", "80", "WPA2", "My:Net"]);
    }
}
