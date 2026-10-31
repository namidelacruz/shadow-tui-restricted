//! `sakura greeter` — Shadow Network login screen for greetd (part of dot//sakura).
//!
//! Talks greetd's IPC (u32 native-endian length + JSON) over $GREETD_SOCK.
//! Failsafes:
//!  * exits non-zero if greetd can't be reached or the protocol misbehaves
//!    three times -> /opt/shadow-tui/lib/greeter-wrapper then runs `agreety`;
//!  * a "Rescue shell" session is always offered (F2) in case the desktop
//!    session is broken;
//!  * on the Linux console only CP437-safe glyphs + 16 programmed palette
//!    slots are used, and the palette is reset on exit.
//! `--demo` previews it without greetd (password: sakura).
//! `--self-test` checks themes/sessions and exits.
use crate::common::*;
use crate::json::{self, Value};
use crate::term::{Input, Key, Style, Term};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

const CONF: &str = "/etc/shadow-tui/greeter.conf";
const STATE_DIR: &str = "/var/cache/shadow-tui-greeter";

#[derive(Clone)]
struct Session {
    name: String,
    cmd: String,
    env: Vec<String>,
}

#[derive(PartialEq, Clone, Copy)]
enum Focus {
    User,
    Pass,
    Session,
}

struct Greeter {
    theme: Theme,
    ui: Ui,
    safe: bool,
    sessions: Vec<Session>,
    sess: usize,
    users: Vec<String>,
    user: Input,
    pass: Input,
    focus: Focus,
    prompt: String,
    msg: (String, bool), // text, is_error
    conn: Option<UnixStream>,
    awaiting: bool, // greetd is waiting for an answer to a prompt
    ipc_failures: u32,
    demo: bool,
    confirm_power: Option<(&'static str, Instant)>,
    tick: u64,
    hostname: String,
    warning: String,
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.truncate(4);
    out
}

fn conf() -> Vec<(String, String)> {
    fs::read_to_string(CONF)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with('#') {
                return None;
            }
            l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        })
        .collect()
}

fn conf_get(c: &[(String, String)], k: &str) -> Option<String> {
    c.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty())
}

fn load_sessions() -> Vec<Session> {
    let mut out = vec![];
    let mut files: Vec<_> = fs::read_dir("/usr/share/wayland-sessions")
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_else(|_| vec![]);
    files.sort();
    for p in files {
        if p.extension().map(|x| x != "desktop").unwrap_or(true) {
            continue;
        }
        let text = fs::read_to_string(&p).unwrap_or_default();
        let get = |k: &str| {
            text.lines().find_map(|l| l.strip_prefix(&format!("{k}=")).map(|v| v.trim().to_string()))
        };
        if let (Some(name), Some(exec)) = (get("Name"), get("Exec")) {
            let id = p.file_stem().unwrap().to_string_lossy().to_string();
            let mut env = vec!["XDG_SESSION_TYPE=wayland".to_string(), format!("XDG_SESSION_DESKTOP={id}")];
            if let Some(dn) = get("DesktopNames") {
                env.push(format!("XDG_CURRENT_DESKTOP={}", dn.trim_end_matches(';').replace(';', ":")));
            }
            out.push(Session { name, cmd: exec, env });
        }
    }
    // our own session first
    out.sort_by_key(|s| if s.cmd.contains("shadow-session") { 0 } else { 1 });
    out.push(Session { name: "Rescue shell (TTY)".into(), cmd: "${SHELL:-/bin/bash} -l".into(), env: vec![] });
    out
}

fn load_users() -> Vec<String> {
    fs::read_to_string("/etc/passwd")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            if f.len() < 7 {
                return None;
            }
            let uid: u32 = f[2].parse().ok()?;
            let shell = f[6];
            if (1000..60000).contains(&uid) && !shell.ends_with("nologin") && !shell.ends_with("false") {
                Some(f[0].to_string())
            } else {
                None
            }
        })
        .collect()
}

fn hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .or_else(|_| fs::read_to_string("/proc/sys/kernel/hostname"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "localhost".into())
}

#[repr(C)]
struct Tm {
    sec: i32,
    min: i32,
    hour: i32,
    mday: i32,
    mon: i32,
    year: i32,
    wday: i32,
    yday: i32,
    isdst: i32,
    gmtoff: i64,
    zone: *const i8,
}
extern "C" {
    fn time(t: *mut i64) -> i64;
    fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
}

fn clock() -> (String, String) {
    unsafe {
        let now = time(std::ptr::null_mut());
        let mut tm: Tm = std::mem::zeroed();
        localtime_r(&now, &mut tm);
        let days = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];
        let mons = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
        (
            format!("{:02}:{:02}:{:02}", tm.hour, tm.min, tm.sec),
            format!("{} {:02} {} {}", days[(tm.wday as usize) % 7], tm.mday, mons[(tm.mon as usize) % 12], 1900 + tm.year),
        )
    }
}

const BANNER: [[&str; 5]; 6] = [
    [" ████", "█    ", " ███ ", "    █", "████ "],
    ["█   █", "█   █", "█████", "█   █", "█   █"],
    [" ███ ", "█   █", "█████", "█   █", "█   █"],
    ["████ ", "█   █", "█   █", "█   █", "████ "],
    [" ███ ", "█   █", "█   █", "█   █", " ███ "],
    ["█   █", "█   █", "█ █ █", "██ ██", "█   █"],
];
const DEFAULT_WARNING: &str =
    "AUTHORIZED ACCESS ONLY. Activity on this system is monitored and logged; unauthorized use is prohibited.";

fn banner_width() -> usize {
    BANNER.iter().map(|g| g[0].chars().count() + 1).sum::<usize>() - 1
}

// ------------------------------------------------------------------ IPC
fn send(conn: &mut UnixStream, v: &Value) -> std::io::Result<()> {
    let body = v.pretty();
    conn.write_all(&(body.len() as u32).to_ne_bytes())?;
    conn.write_all(body.as_bytes())?;
    conn.flush()
}

fn recv(conn: &mut UnixStream) -> std::io::Result<Value> {
    let mut len = [0u8; 4];
    conn.read_exact(&mut len)?;
    let n = u32::from_ne_bytes(len) as usize;
    if n > 1 << 20 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "oversized greetd message"));
    }
    let mut buf = vec![0u8; n];
    conn.read_exact(&mut buf)?;
    json::parse(&String::from_utf8_lossy(&buf)).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}
fn s(x: &str) -> Value {
    Value::Str(x.to_string())
}

impl Greeter {
    fn remember(&self) {
        let _ = fs::create_dir_all(STATE_DIR);
        let _ = fs::write(
            format!("{STATE_DIR}/last"),
            format!("user={}\nsession={}\n", self.user.buf, self.sessions[self.sess].name),
        );
    }

    fn ipc_fail(&mut self, what: String) -> Option<i32> {
        self.conn = None;
        self.awaiting = false;
        self.ipc_failures += 1;
        self.msg = (format!("greetd: {what}"), true);
        if self.ipc_failures >= 3 {
            return Some(3); // let the wrapper fall back to agreety
        }
        None
    }

    /// Drive the greetd conversation as far as possible without user input.
    /// Returns Some(exit_code) when the greeter should exit.
    fn pump(&mut self, mut req: Value) -> Option<i32> {
        loop {
            let conn = self.conn.as_mut().unwrap();
            if let Err(e) = send(conn, &req) {
                return self.ipc_fail(e.to_string());
            }
            let resp = match recv(conn) {
                Ok(v) => v,
                Err(e) => return self.ipc_fail(e.to_string()),
            };
            match resp.path_str(&["type"]).as_deref() {
                Some("success") => {
                    if req.path_str(&["type"]).as_deref() == Some("start_session") {
                        self.remember();
                        return Some(0);
                    }
                    let se = self.sessions[self.sess].clone();
                    req = obj(vec![
                        ("type", s("start_session")),
                        ("cmd", Value::Arr(vec![s(&se.cmd)])),
                        ("env", Value::Arr(se.env.iter().map(|e| s(e)).collect())),
                    ]);
                    self.msg = (format!("starting {}…", se.name), false);
                }
                Some("auth_message") => {
                    let kind = resp.path_str(&["auth_message_type"]).unwrap_or_default();
                    let text = resp.path_str(&["auth_message"]).unwrap_or_default();
                    match kind.as_str() {
                        "secret" | "visible" => {
                            // first password prompt: answer with what was typed
                            if kind == "secret" && !self.pass.buf.is_empty() && !self.awaiting {
                                let pw = std::mem::take(&mut self.pass.buf);
                                self.pass.pos = 0;
                                self.awaiting = true;
                                req = obj(vec![("type", s("post_auth_message_response")), ("response", s(&pw))]);
                                continue;
                            }
                            self.prompt = text.trim().trim_end_matches(':').to_string();
                            self.awaiting = true;
                            self.focus = Focus::Pass;
                            self.pass.set("");
                            return None;
                        }
                        _ => {
                            self.msg = (text, kind == "error");
                            req = obj(vec![("type", s("post_auth_message_response")), ("response", Value::Null)]);
                        }
                    }
                }
                Some("error") => {
                    let et = resp.path_str(&["error_type"]).unwrap_or_default();
                    let d = resp.path_str(&["description"]).unwrap_or_default();
                    self.msg = (if et == "auth_error" { "Access denied — check the password".into() } else { d }, true);
                    let _ = send(self.conn.as_mut().unwrap(), &obj(vec![("type", s("cancel_session"))]));
                    let _ = recv(self.conn.as_mut().unwrap());
                    self.awaiting = false;
                    self.prompt.clear();
                    self.pass.set("");
                    self.focus = Focus::Pass;
                    return None;
                }
                _ => return self.ipc_fail("unexpected reply".into()),
            }
        }
    }

    fn submit(&mut self, t: &mut Term) -> Option<i32> {
        if self.user.buf.trim().is_empty() {
            self.focus = Focus::User;
            self.msg = ("enter a user name".into(), true);
            return None;
        }
        self.msg = ("authenticating…".into(), false);
        self.draw(t);
        let _ = t.present();
        if self.demo {
            if self.pass.buf == "sakura" {
                self.msg = (format!("demo: would start '{}' — Esc to exit", self.sessions[self.sess].cmd), false);
            } else {
                self.msg = ("Access denied — demo password is 'sakura'".into(), true);
            }
            self.pass.set("");
            return None;
        }
        if self.awaiting {
            let ans = std::mem::take(&mut self.pass.buf);
            self.pass.pos = 0;
            return self.pump(obj(vec![("type", s("post_auth_message_response")), ("response", s(&ans))]));
        }
        if self.conn.is_none() {
            let path = std::env::var("GREETD_SOCK").unwrap_or_default();
            match UnixStream::connect(&path) {
                Ok(c) => {
                    let _ = c.set_read_timeout(Some(Duration::from_secs(60)));
                    self.conn = Some(c);
                }
                Err(e) => return self.ipc_fail(format!("cannot connect ({e})")),
            }
        }
        self.pump(obj(vec![("type", s("create_session")), ("username", s(self.user.buf.trim()))]))
    }

    // ------------------------------------------------------------ drawing
    fn draw(&mut self, t: &mut Term) {
        let th = &self.theme;
        let (w, h) = (t.w, t.h);
        let bg = Style::new(th.c("fg"), th.c("bg"));
        t.clear(bg);
        t.cursor = None;
        self.backdrop(t);

        let pw = 56.min(w.saturating_sub(4));
        let ph = 21.min(h.saturating_sub(2));
        let px = (w - pw) / 2;
        let py = (h.saturating_sub(ph)) / 2;
        let panel = Style::new(th.c("fg"), th.c("bg_alt"));
        t.fill(px, py, pw, ph, panel);
        t.frame(px, py, pw, ph, self.ui.glyphs, Style::new(th.c("accent"), th.c("bg_alt")));
        if th.bar_style == "bracket" {
            for (x, y) in [(px, py), (px + pw - 1, py), (px, py + ph - 1), (px + pw - 1, py + ph - 1), (px + pw / 2, py), (px + pw / 2, py + ph - 1)] {
                t.put(x, y, '♦', Style::new(th.c("accent"), th.c("bg_alt")));
            }
        }
        // banner
        let bw = banner_width();
        if pw > bw + 4 {
            let mut bx = px + (pw - bw) / 2;
            for (gi, g) in BANNER.iter().enumerate() {
                let col = if gi % 2 == 0 { th.c("accent") } else { th.c("accent").mix(th.c("accent2"), 0.5) };
                for (row, line) in g.iter().enumerate() {
                    for (i, ch) in line.chars().enumerate() {
                        if ch != ' ' {
                            t.put(bx + i, py + 2 + row, '█', Style::new(col, th.c("bg_alt")));
                        }
                    }
                }
                bx += g[0].chars().count() + 1;
            }
        }
        let net = "N  E  T  W  O  R  K";
        let nx = px + (pw.saturating_sub(net.chars().count())) / 2;
        t.text(nx, py + 7, net, Style::new(th.c("accent2"), th.c("bg_alt")).bold(), pw - 2);
        let sub = match th.bar_style.as_str() {
            "tactical" => format!("NODE {} :: SECURE UPLINK", self.hostname.to_uppercase()),
            "bracket" => format!("◄ {} · AREA SERVER ►", self.hostname.to_uppercase()),
            "slash" => format!("// {} :: NETRUNNER ACCESS", self.hostname.to_uppercase()),
            _ => format!("GHOST LINE :: {}", self.hostname.to_uppercase()),
        };
        let sx = px + (pw.saturating_sub(sub.chars().count())) / 2;
        t.text(sx, py + 9, &sub, Style::new(th.c("fg_dim"), th.c("bg_alt")), pw - 2);

        // fields
        let lx = px + 4;
        let fx = px + 15;
        let fw = pw.saturating_sub(19);
        let label = |f: Focus, me: &Greeter| {
            if me.focus == f { Style::new(th.c("accent"), th.c("bg_alt")).bold() } else { Style::new(th.c("fg_dim"), th.c("bg_alt")) }
        };
        let field = Style::new(th.c("fg"), th.c("surface"));
        let fieldf = Style::new(th.c("fg"), th.c("bg"));
        t.text(lx, py + 12, "USER", label(Focus::User, self), 10);
        self.user.draw(t, fx, py + 12, fw, if self.focus == Focus::User { fieldf } else { field }, self.focus == Focus::User, None);
        let plabel = if self.prompt.is_empty() { "PASSWORD".to_string() } else { self.prompt.to_uppercase() };
        t.text(lx, py + 14, &plabel, label(Focus::Pass, self), 10);
        let mask = if self.prompt.is_empty() || self.prompt.to_lowercase().contains("password") { Some('*') } else { None };
        self.pass.draw(t, fx, py + 14, fw, if self.focus == Focus::Pass { fieldf } else { field }, self.focus == Focus::Pass, mask);
        t.text(lx, py + 16, "SESSION", label(Focus::Session, self), 10);
        let sname = format!("◄ {} ►", self.sessions[self.sess].name);
        t.fill(fx, py + 16, fw, 1, if self.focus == Focus::Session { fieldf } else { field });
        t.text(fx + 1, py + 16, &sname, if self.focus == Focus::Session { Style::new(th.c("accent"), th.c("bg")).bold() } else { field }, fw - 1);
        // message
        let (m, err) = &self.msg;
        let ms = if *err { Style::new(th.c("alert"), th.c("bg_alt")).bold() } else { Style::new(th.c("accent2"), th.c("bg_alt")) };
        let mx = px + (pw.saturating_sub(m.chars().count())) / 2;
        t.text(mx.max(px + 2), py + 18, m, ms, pw - 4);
        if self.demo {
            t.text(px + 2, py + ph - 1, " DEMO ", Style::new(th.c("bg"), th.c("warn")).bold(), 6);
        }

        // warning banner
        if !self.warning.is_empty() {
            let ww = (pw + 8).min(w.saturating_sub(2));
            let lines = wrap(&self.warning, ww.saturating_sub(4));
            let wh = lines.len() + 2;
            let wx = (w - ww) / 2;
            let wy = if py >= wh + 2 { py - wh - 1 } else { 1 };
            let st = Style::new(th.c("bg"), th.c("alert")).bold();
            t.fill(wx, wy, ww, wh, st);
            let head = "▲ WARNING ▲";
            t.text(wx + (ww.saturating_sub(head.chars().count())) / 2, wy, head, st, ww);
            for (i, l) in lines.iter().enumerate() {
                t.text(wx + (ww.saturating_sub(l.chars().count())) / 2, wy + 1 + i, l, Style::new(th.c("bg"), th.c("alert")), ww);
            }
        }

        // bottom bar
        let (tm, date) = clock();
        let bar = Style::new(th.c("bg"), th.c("accent"));
        t.fill(0, h - 1, w, 1, bar);
        t.text(1, h - 1, &format!(" {tm}  {date} "), bar.bold(), w);
        let keys = match self.confirm_power {
            Some((what, _)) => format!("press again to {what} · any other key cancels "),
            None => "Tab next · ←→ session · F2 rescue shell · F11 reboot · F12 power off ".to_string(),
        };
        t.text(w.saturating_sub(keys.chars().count() + 1), h - 1, &keys, bar, w);
        if th.bar_style != "tactical" {
            t.text(1, 0, &format!("{} ", th.name.to_uppercase()), Style::new(th.c("fg_dim"), th.c("bg")), w);
        }
    }

    fn backdrop(&self, t: &mut Term) {
        let th = &self.theme;
        let (w, h) = (t.w, t.h);
        let dimc = th.c("bg").mix(th.c("accent"), 0.22);
        let dim2 = th.c("bg").mix(th.c("accent2"), 0.3);
        let mut rng = 0x2545_f491u64;
        let mut rnd = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        match th.bar_style.as_str() {
            "tactical" => {
                // map grid with coordinates
                let grid = Style::new(th.c("bg").mix(th.c("accent"), 0.18), th.c("bg"));
                let lab = Style::new(th.c("bg").mix(th.c("fg_dim"), 0.8), th.c("bg"));
                for y in 1..h.saturating_sub(1) {
                    for x in 0..w {
                        let gx = x % 12 == 0;
                        let gy = y % 6 == 0;
                        let ch = match (gx, gy) {
                            (true, true) => '+',
                            (true, false) => '·',
                            (false, true) => '·',
                            _ => continue,
                        };
                        if (gx && y % 2 == 0) || (gy && x % 3 == 0) || (gx && gy) {
                            t.put(x, y, ch, grid);
                        }
                    }
                }
                for (i, x) in (0..w).step_by(12).enumerate().skip(1) {
                    t.text(x + 1, 1, &format!("{:03}", 400 + i), lab, 3);
                }
                for (i, y) in (0..h).step_by(6).enumerate().skip(1) {
                    t.text(1, y.saturating_sub(1), &format!("{:03}", 120 + i), lab, 3);
                }
                // radar: range ring + rotating sweep at the right
                // fit the radar into the free space right of the login panel
                let pw = 56.min(w.saturating_sub(4));
                let right = (w - pw) / 2 + pw;
                let avail = w.saturating_sub(right) as f32;
                let (cx, cy) = (right as f32 + avail / 2.0, h as f32 * 0.5);
                let r = (h as f32 * 0.32).min((avail - 4.0) / 4.0);
                let ring = Style::new(th.c("bg").mix(th.c("accent"), 0.45), th.c("bg"));
                let radar = r >= 4.0;
                for k in 0..(if radar { 120 } else { 0 }) {
                    let a = k as f32 / 120.0 * std::f32::consts::TAU;
                    let (x, y) = (cx + a.cos() * r * 2.0, cy + a.sin() * r);
                    if x >= 0.0 && y >= 1.0 && (x as usize) < w && (y as usize) < h - 1 {
                        t.put(x as usize, y as usize, '·', ring);
                    }
                }
                let sweep = (self.tick as f32 * 0.06) % std::f32::consts::TAU;
                for trail in 0..(if radar { 6 } else { 0 }) {
                    let a = sweep - trail as f32 * 0.08;
                    let st = Style::new(th.c("bg").mix(th.c("accent"), 0.9 - trail as f32 * 0.13), th.c("bg"));
                    for k in 1..(r as usize) {
                        let (x, y) = (cx + a.cos() * k as f32 * 2.0, cy + a.sin() * k as f32);
                        if x >= 0.0 && y >= 1.0 && (x as usize) < w && (y as usize) < h - 1 {
                            t.put(x as usize, y as usize, if trail == 0 { '█' } else { '░' }, st);
                        }
                    }
                }
                for (dx, dy, hostile) in if radar { vec![(0.5f32, -0.4f32, false), (-0.6, 0.3, false), (0.2, 0.6, true)] } else { vec![] } {
                    let (x, y) = ((cx + dx * r * 2.0) as usize, (cy + dy * r) as usize);
                    if x < w && y < h - 1 {
                        t.put(x, y, if hostile { '◊' } else { '•' }, Style::new(if hostile { th.c("alert") } else { th.c("accent2") }, th.c("bg")));
                    }
                }
                // hologlobe: rotating 3D wireframe sphere in the free space left of the panel
                let left = (w - pw) / 2;
                let rows = (h as f32 * 0.24).min((left as f32 - 12.0) / 4.6);
                if rows >= 3.0 {
                    let (gx, gy) = (left as f32 / 2.0, h as f32 * 0.48);
                    hologlobe(t, gx, gy, rows, self.tick,
                        Style::new(th.c("accent"), th.c("bg")),
                        Style::new(th.c("bg").mix(th.c("accent"), 0.35), th.c("bg")),
                        Style::new(th.c("accent2"), th.c("bg")));
                    let label = if self.tick < 60 { "HOLOGLOBE // SYNCING" } else { "HOLOGLOBE // NODE LINK" };
                    let lx = (gx as usize).saturating_sub(label.len() / 2);
                    let ly = (gy + rows + 1.5) as usize;
                    if ly < h - 1 {
                        t.text(lx, ly, label, lab, left);
                    }
                }
                // classification strip on the top row
                let cls = "RESTRICTED // SHADOW NETWORK // TACTICAL NET";
                let st = Style::new(th.c("bg"), th.c("accent2")).bold();
                t.fill(0, 0, w, 1, st);
                t.text(w.saturating_sub(cls.len()) / 2, 0, cls, st, w);
            }
            "frame" => {
                // slow hex rain
                let hexd: Vec<char> = "0123456789ABCDEF".chars().collect();
                for x in (0..w).step_by(3) {
                    if rnd() % 3 != 0 {
                        continue;
                    }
                    let len = 4 + (rnd() % 14) as usize;
                    let speed = 1 + rnd() % 3;
                    let head = ((self.tick / speed) as usize + (rnd() as usize % h.max(1))) % (h + len);
                    for i in 0..len {
                        if head >= i && head - i < h.saturating_sub(1) {
                            let y = head - i;
                            let c = hexd[((rnd() as usize) + y) % 16];
                            let col = if i == 0 { th.c("accent") } else { th.c("bg").mix(th.c("accent"), 0.45 - (i as f32 / len as f32) * 0.35) };
                            t.put(x, y, c, Style::new(col, th.c("bg")));
                        }
                    }
                }
            }
            "bracket" => {
                for _ in 0..(w * h / 40) {
                    let x = (rnd() as usize) % w;
                    let y = (rnd() as usize) % h.saturating_sub(1).max(1);
                    let ch = ['·', '*', '·', '+'][(rnd() % 4) as usize];
                    let twinkle = (self.tick / 5 + rnd() % 7) % 7 == 0;
                    t.put(x, y, ch, Style::new(if twinkle { th.c("accent") } else { dimc }, th.c("bg")));
                }
                // horizon waves
                for x in 0..w {
                    let y = h.saturating_sub(4) + (((x as f32 / 6.0 + self.tick as f32 / 8.0).sin() + 1.0) * 1.2) as usize;
                    if y < h - 1 {
                        t.put(x, y, '~', Style::new(dim2, th.c("bg")));
                    }
                }
            }
            _ => {
                // hazard stripes top-left + glitch bars
                for y in 0..(h / 4).max(2) {
                    for x in 0..(w / 3) {
                        if (x + y) % 6 < 3 && x + y * 2 < w / 3 {
                            t.put(x, y, '/', Style::new(th.c("accent"), th.c("bg")));
                        }
                    }
                }
                let phase = self.tick / 3;
                for i in 0..6u64 {
                    let y = ((rnd() + phase * (i + 1)) as usize) % h.saturating_sub(1).max(1);
                    let x = (rnd() as usize) % w;
                    let len = 4 + (rnd() as usize) % 20;
                    let col = if i % 2 == 0 { th.c("alert") } else { th.c("accent2") };
                    for xx in x..(x + len).min(w) {
                        t.put(xx, y, '▀', Style::new(th.c("bg").mix(col, 0.6), th.c("bg")));
                    }
                }
                let label = "SYS//ONLINE";
                t.text(w.saturating_sub(label.len() + 2), h.saturating_sub(3), label, Style::new(th.c("alert"), th.c("bg")).bold(), w);
            }
        }
    }

    fn key(&mut self, k: Key, t: &mut Term) -> Option<i32> {
        if let Some((what, at)) = self.confirm_power.take() {
            let again = matches!((what, k), ("reboot", Key::F(11)) | ("power off", Key::F(12)));
            if again && at.elapsed() < Duration::from_secs(5) {
                if !self.demo {
                    let _ = cmd_out(&["systemctl", if what == "reboot" { "reboot" } else { "poweroff" }]);
                }
                self.msg = (format!("{what}…"), false);
                return None;
            }
        }
        match k {
            Key::F(11) => self.confirm_power = Some(("reboot", Instant::now())),
            Key::F(12) => self.confirm_power = Some(("power off", Instant::now())),
            Key::F(2) => {
                self.sess = self.sessions.len() - 1;
                self.msg = ("rescue shell selected — log in to get a TTY shell".into(), false);
            }
            Key::Esc if self.demo => return Some(0),
            Key::Esc => {
                if self.awaiting {
                    if let Some(c) = self.conn.as_mut() {
                        let _ = send(c, &obj(vec![("type", s("cancel_session"))]));
                        let _ = recv(c);
                    }
                    self.awaiting = false;
                    self.prompt.clear();
                }
                self.pass.set("");
                self.msg = (String::new(), false);
            }
            Key::Tab | Key::Down => {
                self.focus = match self.focus {
                    Focus::User => Focus::Pass,
                    Focus::Pass => Focus::Session,
                    Focus::Session => Focus::User,
                }
            }
            Key::BackTab | Key::Up => {
                self.focus = match self.focus {
                    Focus::User => Focus::Session,
                    Focus::Pass => Focus::User,
                    Focus::Session => Focus::Pass,
                }
            }
            Key::Enter => match self.focus {
                Focus::User => self.focus = Focus::Pass,
                _ => return self.submit(t),
            },
            Key::Left | Key::Right if self.focus == Focus::Session => {
                let n = self.sessions.len();
                self.sess = if k == Key::Left { (self.sess + n - 1) % n } else { (self.sess + 1) % n };
            }
            Key::Left | Key::Right if self.focus == Focus::User && self.user.buf.is_empty() || (self.focus == Focus::User && self.users.contains(&self.user.buf)) => {
                if !self.users.is_empty() {
                    let i = self.users.iter().position(|u| *u == self.user.buf).map(|i| i as i64).unwrap_or(-1);
                    let n = self.users.len() as i64;
                    let j = if k == Key::Left { (i - 1 + n) % n } else { (i + 1) % n };
                    self.user.set(&self.users[j as usize].clone());
                }
            }
            other => match self.focus {
                Focus::User => {
                    self.user.handle(other);
                }
                Focus::Pass => {
                    self.pass.handle(other);
                }
                Focus::Session => {}
            },
        }
        None
    }
}

pub fn run(args: &[String]) -> i32 {
    let c = conf();
    let demo = args.iter().any(|a| a == "--demo");
    let self_test = args.iter().any(|a| a == "--self-test");
    let theme_arg = args.windows(2).find(|w| w[0] == "--theme").map(|w| w[1].clone());
    let theme_id = theme_arg.or_else(|| conf_get(&c, "theme")).unwrap_or_else(|| "nightwatch".into());
    let theme = find_theme(&theme_id);
    let sessions = load_sessions();
    let users = load_users();

    if self_test {
        println!("theme      : {} ({})", theme.name, theme.id);
        println!("themes     : {}", list_themes().iter().map(|t| t.id.clone()).collect::<Vec<_>>().join(", "));
        println!("sessions   : {}", sessions.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(" | "));
        println!("users      : {}", users.len());
        println!("warning    : {}", c.iter().find(|(k, _)| k == "warning").map(|(_, v)| if v.is_empty() { "(hidden)".to_string() } else { v.clone() }).unwrap_or_else(|| "(default)".into()));
        println!("greetd sock: {}", std::env::var("GREETD_SOCK").unwrap_or_else(|_| "(not set — fine outside greetd)".into()));
        let ok = theme.colors.iter().all(|(_, h)| valid_hex(h)) && !sessions.is_empty();
        println!("result     : {}", if ok { "OK" } else { "FAIL" });
        return if ok { 0 } else { 1 };
    }
    if !demo && std::env::var("GREETD_SOCK").is_err() {
        eprintln!("shadow greeter: GREETD_SOCK not set (run under greetd, or use --demo)");
        return 2;
    }

    // last user / session, then config defaults
    let last = fs::read_to_string(format!("{STATE_DIR}/last")).unwrap_or_default();
    let last_get = |k: &str| last.lines().find_map(|l| l.strip_prefix(&format!("{k}=")).map(String::from)).filter(|v| !v.is_empty());
    let user = last_get("user").or_else(|| conf_get(&c, "default_user")).or_else(|| if users.len() == 1 { Some(users[0].clone()) } else { None }).unwrap_or_default();
    let want_sess = last_get("session").or_else(|| conf_get(&c, "default_session"));
    let sess = want_sess.and_then(|n| sessions.iter().position(|s| s.name == n || s.cmd.contains(&n))).unwrap_or(0);

    let safe = Term::linux_console();
    let mut t = match Term::enter(Some(theme.vt_slots())) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("shadow greeter: {e}");
            return 2;
        }
    };
    let mut g = Greeter {
        ui: Ui::new(&theme, safe),
        theme,
        safe,
        sessions,
        sess,
        users,
        focus: if user.is_empty() { Focus::User } else { Focus::Pass },
        user: Input::with(&user),
        pass: Input::default(),
        prompt: String::new(),
        msg: (if demo { "demo mode — password: sakura".into() } else { String::new() }, false),
        conn: None,
        awaiting: false,
        ipc_failures: 0,
        demo,
        confirm_power: None,
        tick: 0,
        hostname: hostname(),
        // `warning=` in greeter.conf: absent -> default text, empty -> hidden
        warning: match c.iter().find(|(k, _)| k == "warning") {
            Some((_, v)) => v.clone(),
            None => DEFAULT_WARNING.to_string(),
        },
    };
    let _ = g.safe;
    loop {
        if t.resize() {
            t.force_redraw();
        }
        g.tick += 1;
        g.draw(&mut t);
        let _ = t.present();
        for k in t.keys() {
            if let Some(code) = g.key(k, &mut t) {
                t.leave();
                return code;
            }
        }
    }
}

/// Rotating wireframe sphere in braille dots (2x4 per cell, roughly square dots).
/// Front-facing lines are bright, the far side dim; rings appear one by one during boot.
fn hologlobe(t: &mut Term, cx: f32, cy: f32, rows: f32, tick: u64, front: Style, back: Style, sat: Style) {
    use std::collections::HashMap;
    let rd = rows * 4.0; // radius in dots
    let theta = tick as f32 * 0.05;
    let tilt: f32 = 0.38;
    let (st, ct) = theta.sin_cos();
    let (sx, cx_) = tilt.sin_cos();
    // cell -> (front bits, back bits)
    let mut cells: HashMap<(i32, i32), (u8, u8)> = HashMap::new();
    let ox = cx * 2.0;
    let oy = cy * 4.0;
    let mut plot = |x: f32, y: f32, z: f32| {
        // rotate about Y (spin) then X (tilt)
        let (x1, z1) = (x * ct + z * st, -x * st + z * ct);
        let (y2, z2) = (y * cx_ - z1 * sx, y * sx + z1 * cx_);
        let dx = (ox + x1 * rd).round() as i32;
        let dy = (oy - y2 * rd).round() as i32;
        if dx < 0 || dy < 0 {
            return;
        }
        let cell = (dx / 2, dy / 4);
        let bit = match (dx % 2, dy % 4) {
            (0, 0) => 0x01, (0, 1) => 0x02, (0, 2) => 0x04, (1, 0) => 0x08,
            (1, 1) => 0x10, (1, 2) => 0x20, (0, _) => 0x40, _ => 0x80,
        };
        let e = cells.entry(cell).or_insert((0, 0));
        if z2 >= 0.0 { e.0 |= bit } else { e.1 |= bit }
    };
    let pi = std::f32::consts::PI;
    let built = (tick / 4) as usize; // boot build-up: one ring every 4 frames
    let steps = (rd * 5.0) as usize;
    let mut ring = 0;
    for lon in 0..8 {
        ring += 1;
        if ring > built { break; }
        let l = lon as f32 / 8.0 * pi;
        for k in 0..steps {
            let a = k as f32 / steps as f32 * 2.0 * pi;
            plot(a.sin() * l.cos(), a.cos(), a.sin() * l.sin());
        }
    }
    for lat in [-60.0f32, -30.0, 0.0, 30.0, 60.0] {
        ring += 1;
        if ring > built { break; }
        let (sl, cl) = lat.to_radians().sin_cos();
        for k in 0..steps {
            let a = k as f32 / steps as f32 * 2.0 * pi;
            plot(cl * a.cos(), sl, cl * a.sin());
        }
    }
    for ((x, y), (f, b)) in &cells {
        let (x, y) = (*x as usize, *y as usize);
        if y < 1 || x >= t.w || y + 1 >= t.h {
            continue;
        }
        let bits = if *f != 0 { *f } else { *b };
        let ch = char::from_u32(0x2800 + bits as u32).unwrap_or(' ');
        t.put(x, y, ch, if *f != 0 { front } else { back });
    }
    // a satellite on an inclined orbit
    let a = tick as f32 * 0.11;
    let (r, inc) = (1.35f32, 0.6f32);
    let (x, y, z) = (r * a.cos(), r * a.sin() * inc.sin(), r * a.sin() * inc.cos());
    let (x1, z1) = (x * ct + z * st, -x * st + z * ct);
    let y2 = y * cx_ - z1 * sx;
    let (px, py) = ((ox + x1 * rd) / 2.0, (oy - y2 * rd) / 4.0);
    if px >= 0.0 && py >= 1.0 && (px as usize) < t.w && (py as usize) + 1 < t.h {
        t.put(px as usize, py as usize, '◆', sat);
    }
}
