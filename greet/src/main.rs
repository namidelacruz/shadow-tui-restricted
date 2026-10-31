//! shadow-greet — the Shadow TUI Restricted login screen.
//!
//! A static console greeter for greetd in the spirit of ly: one box, two fields, no animation,
//! no power buttons and no command hints. It sizes the console font to the panel's pixel density
//! (Terminus via setfont) so the box is the same physical size on any screen, and lays itself
//! out from the terminal size it ends up with.
//!
//!   shadow-greet               run under greetd (GREETD_SOCK)
//!   shadow-greet --demo        try it in any terminal (password: shadow)
//!   shadow-greet --font-only   just pick and set the console font
//!   shadow-greet --self-test   check configuration and exit
//!
//! /etc/shadow-tui/greeter.conf (key = value):
//!   session = shadow-tui        wayland session id to start (default: Shadow TUI, else the first)
//!   console_font = auto         auto | off | a setfont name such as ter-v24n
//!   warning = AUTHORIZED ACCESS ONLY · activity on this system is monitored and logged
//!   title = SHADOW TUI // RESTRICTED
#[allow(dead_code)]
mod json;
#[path = "../../ui/src/scale.rs"]
#[allow(dead_code)]
mod scale;
#[allow(dead_code)]
mod term;

use json::Value;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use term::{Color, Input, Key, Style, Term};

const CONF: &str = "/etc/shadow-tui/greeter.conf";
const STATE_DIR: &str = "/var/cache/shadow-tui-greeter";

const OBSIDIAN: Color = Color::Rgb(0x0A, 0x0B, 0x0E);
const PANEL: Color = Color::Rgb(0x14, 0x16, 0x1B);
const AMBER: Color = Color::Rgb(0xFF, 0xB0, 0x00);
const CRIMSON: Color = Color::Rgb(0xFF, 0x2A, 0x00);
const CYAN: Color = Color::Rgb(0x00, 0xE5, 0xFF);
const TEXT: Color = Color::Rgb(0xE8, 0xE2, 0xD0);
const DIM: Color = Color::Rgb(0x8A, 0x84, 0x74);

/// Linux console: the 16 palette slots, programmed for the theme and reset on exit.
const VT_PALETTE: [Color; 16] = [OBSIDIAN, CRIMSON, Color::Rgb(0x7C, 0xD9, 0x5A), AMBER, Color::Rgb(0x3A, 0x6E, 0xA5), Color::Rgb(0xB0, 0x60, 0xD0), CYAN, TEXT, DIM, CRIMSON, Color::Rgb(0x9C, 0xF9, 0x7A), AMBER, Color::Rgb(0x5A, 0x8E, 0xC5), Color::Rgb(0xD0, 0x80, 0xF0), CYAN, Color::Rgb(0xFF, 0xFF, 0xFF)];

fn conf() -> Vec<(String, String)> {
    let path = std::env::var("SHADOW_GREETER_CONF").unwrap_or_else(|_| CONF.into());
    fs::read_to_string(path)
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

#[derive(Clone, Debug)]
struct Session {
    id: String,
    cmd: String,
    env: Vec<String>,
}

fn load_session(want: Option<&str>) -> Option<Session> {
    let dir = std::env::var("SHADOW_SESSIONS_DIR").unwrap_or_else(|_| "/usr/share/wayland-sessions".into());
    let mut all = vec![];
    let mut files: Vec<_> = fs::read_dir(&dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_else(|_| vec![]);
    files.sort();
    for p in files {
        if p.extension().map_or(true, |x| x != "desktop") {
            continue;
        }
        let text = fs::read_to_string(&p).unwrap_or_default();
        let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}=")).map(|v| v.trim().to_string()));
        if let Some(exec) = get("Exec") {
            let id = p.file_stem().unwrap().to_string_lossy().to_string();
            let mut env = vec!["XDG_SESSION_TYPE=wayland".to_string(), format!("XDG_SESSION_DESKTOP={id}")];
            if let Some(dn) = get("DesktopNames") {
                env.push(format!("XDG_CURRENT_DESKTOP={}", dn.trim_end_matches(';').replace(';', ":")));
            }
            all.push(Session { id, cmd: exec, env });
        }
    }
    if let Some(w) = want {
        if let Some(s) = all.iter().find(|s| s.id == w) {
            return Some(s.clone());
        }
    }
    all.iter().find(|s| s.cmd.contains("shadow-session")).or(all.first()).cloned()
}

fn hostname() -> String {
    fs::read_to_string("/etc/hostname").or_else(|_| fs::read_to_string("/proc/sys/kernel/hostname")).map(|s| s.trim().to_string()).unwrap_or_else(|_| "localhost".into())
}

fn last_user() -> String {
    fs::read_to_string(format!("{STATE_DIR}/last")).ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("user=").map(String::from))).unwrap_or_default()
}

// ------------------------------------------------------------------ console font
/// (pixels w x h, millimetres w x h) of the connected panel, from DRM sysfs + EDID.
fn panel() -> Option<(String, (i32, i32), (i32, i32))> {
    let root = std::env::var("SHADOW_SYS_ROOT").unwrap_or_else(|_| "/".into());
    let drm = std::path::Path::new(&root).join("sys/class/drm");
    let mut conns: Vec<_> = fs::read_dir(&drm).ok()?.flatten().map(|e| e.path()).filter(|p| p.join("status").exists()).collect();
    // the built-in panel first
    conns.sort_by_key(|p| {
        let n = p.file_name().unwrap().to_string_lossy().to_string();
        (!n.contains("eDP"), n)
    });
    for c in conns {
        if fs::read_to_string(c.join("status")).map(|s| s.trim() != "connected").unwrap_or(true) {
            continue;
        }
        let name = c.file_name()?.to_string_lossy().split_once('-').map(|(_, n)| n.to_string()).unwrap_or_default();
        let mode = fs::read_to_string(c.join("modes")).ok()?.lines().next()?.to_string();
        let (w, h) = mode.split_once('x')?;
        let px = (w.parse().ok()?, h.trim_end_matches(|c: char| !c.is_ascii_digit()).parse().ok()?);
        let edid = fs::read(c.join("edid")).unwrap_or_default();
        let mm = if edid.len() >= 128 {
            // detailed timing descriptor 1 has mm precision; the basic block only cm
            let d = &edid[54..72];
            let (hm, vm) = (d[12] as i32 | ((d[14] as i32 & 0xF0) << 4), d[13] as i32 | ((d[14] as i32 & 0x0F) << 8));
            if hm > 0 && vm > 0 {
                (hm, vm)
            } else {
                (edid[21] as i32 * 10, edid[22] as i32 * 10)
            }
        } else {
            (0, 0)
        };
        return Some((name, px, mm));
    }
    None
}

/// Terminus height for the panel: 16 px at scale 1, in the sizes Terminus ships.
fn font_for(scale: f64) -> String {
    let want = 16.0 * scale;
    let sizes = [12, 14, 16, 18, 20, 22, 24, 28, 32];
    let best = sizes.iter().min_by_key(|s| ((**s as f64 - want).abs() * 10.0) as i64).copied().unwrap_or(16);
    format!("ter-v{best}n")
}

fn set_console_font(c: &[(String, String)]) -> Option<String> {
    let mode = conf_get(c, "console_font").unwrap_or_else(|| "auto".into());
    if mode == "off" || !Term::linux_console() {
        return None;
    }
    let font = if mode == "auto" {
        let (name, px, mm) = panel()?;
        font_for(scale::ideal_scale(&name, px, mm).unwrap_or(if px.1 >= 1400 { 1.5 } else { 1.0 }))
    } else {
        mode
    };
    let ok = std::process::Command::new("setfont").arg(&font).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
    ok.then_some(font)
}

// ------------------------------------------------------------------ greetd IPC
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

/// While a system update runs (or an interrupted one awaits recovery) no desktop session starts.
const UPDATE_MARK: &str = "/var/lib/shadow-tui/update-in-progress";

fn update_busy() -> bool {
    std::path::Path::new(&std::env::var("SHADOW_UPDATE_MARK").unwrap_or_else(|_| UPDATE_MARK.into())).exists()
}

const UPDATE_MSG: &str = "system update in progress — login paused until it finishes";

#[derive(PartialEq, Clone, Copy)]
enum Field {
    User,
    Pass,
}

struct Greeter {
    title: String,
    warning: String,
    host: String,
    session: Option<Session>,
    user: Input,
    pass: Input,
    field: Field,
    prompt: String,
    msg: (String, bool),
    conn: Option<UnixStream>,
    awaiting: bool,
    failures: u32,
    demo: bool,
}

impl Greeter {
    fn fail(&mut self, what: String) -> Option<i32> {
        self.conn = None;
        self.awaiting = false;
        self.failures += 1;
        self.msg = (format!("greetd: {what}"), true);
        (self.failures >= 3).then_some(3) // the wrapper falls back to agreety
    }

    fn pump(&mut self, mut req: Value) -> Option<i32> {
        loop {
            let conn = self.conn.as_mut()?;
            if let Err(e) = send(conn, &req) {
                return self.fail(e.to_string());
            }
            let resp = match recv(conn) {
                Ok(v) => v,
                Err(e) => return self.fail(e.to_string()),
            };
            match resp.path_str(&["type"]).as_deref() {
                Some("success") => {
                    if req.path_str(&["type"]).as_deref() == Some("start_session") {
                        let _ = fs::create_dir_all(STATE_DIR);
                        let _ = fs::write(format!("{STATE_DIR}/last"), format!("user={}\n", self.user.buf.trim()));
                        return Some(0);
                    }
                    if update_busy() {
                        // authenticated, but the system is mid-update: don't start anything
                        if let Some(c) = self.conn.as_mut() {
                            let _ = send(c, &obj(vec![("type", s("cancel_session"))]));
                            let _ = recv(c);
                        }
                        self.awaiting = false;
                        self.pass.set("");
                        self.msg = (UPDATE_MSG.into(), true);
                        return None;
                    }
                    let Some(se) = self.session.clone() else {
                        self.msg = ("no desktop session installed".into(), true);
                        return None;
                    };
                    req = obj(vec![("type", s("start_session")), ("cmd", Value::Arr(vec![s(&se.cmd)])), ("env", Value::Arr(se.env.iter().map(|e| s(e)).collect()))]);
                    self.msg = ("starting session".into(), false);
                }
                Some("auth_message") => {
                    let kind = resp.path_str(&["auth_message_type"]).unwrap_or_default();
                    let text = resp.path_str(&["auth_message"]).unwrap_or_default();
                    match kind.as_str() {
                        "secret" | "visible" => {
                            if kind == "secret" && !self.pass.buf.is_empty() && !self.awaiting {
                                let pw = std::mem::take(&mut self.pass.buf);
                                self.pass.pos = 0;
                                self.awaiting = true;
                                req = obj(vec![("type", s("post_auth_message_response")), ("response", s(&pw))]);
                                continue;
                            }
                            // a second factor (token, PIN): ask with greetd's own words
                            self.prompt = text.trim().trim_end_matches(':').to_string();
                            self.awaiting = true;
                            self.field = Field::Pass;
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
                    self.msg = (if et == "auth_error" { "access denied".into() } else { d }, true);
                    if let Some(c) = self.conn.as_mut() {
                        let _ = send(c, &obj(vec![("type", s("cancel_session"))]));
                        let _ = recv(c);
                    }
                    self.awaiting = false;
                    self.prompt.clear();
                    self.pass.set("");
                    self.field = Field::Pass;
                    return None;
                }
                _ => return self.fail("unexpected reply".into()),
            }
        }
    }

    fn submit(&mut self) -> Option<i32> {
        if update_busy() && !self.awaiting {
            self.pass.set("");
            self.msg = (UPDATE_MSG.into(), true);
            return None;
        }
        if self.user.buf.trim().is_empty() {
            self.field = Field::User;
            self.msg = ("enter a login".into(), true);
            return None;
        }
        if self.demo {
            self.msg = if self.pass.buf == "shadow" { (format!("demo: would start {}", self.session.as_ref().map(|s| s.id.as_str()).unwrap_or("?")), false) } else { ("access denied".into(), true) };
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
                Err(e) => return self.fail(format!("cannot connect ({e})")),
            }
        }
        self.pump(obj(vec![("type", s("create_session")), ("username", s(self.user.buf.trim()))]))
    }

    fn key(&mut self, k: Key) -> Option<i32> {
        self.msg.0.clear();
        match k {
            Key::Tab | Key::BackTab | Key::Up | Key::Down => {
                if !self.awaiting {
                    self.field = if self.field == Field::User { Field::Pass } else { Field::User };
                }
            }
            Key::Enter => {
                if self.field == Field::User && self.pass.buf.is_empty() && !self.awaiting {
                    self.field = Field::Pass;
                } else {
                    return self.submit();
                }
            }
            Key::Esc => {
                self.pass.set("");
                if self.awaiting {
                    if let Some(c) = self.conn.as_mut() {
                        let _ = send(c, &obj(vec![("type", s("cancel_session"))]));
                        let _ = recv(c);
                    }
                    self.awaiting = false;
                    self.prompt.clear();
                }
            }
            Key::Ctrl('c') if self.demo => return Some(0),
            k => {
                let inp = if self.field == Field::User { &mut self.user } else { &mut self.pass };
                inp.handle(k);
            }
        }
        None
    }

    fn draw(&self, t: &mut Term) {
        let bg = Style::new(TEXT, OBSIDIAN);
        t.clear(bg);
        t.cursor = None;
        let w = t.w.saturating_sub(4).clamp(20, 64);
        let h = 11;
        let x = t.w.saturating_sub(w) / 2;
        let y = t.h.saturating_sub(h) / 2;
        let panel = Style::new(TEXT, PANEL);
        t.fill(x, y, w, h, panel);
        t.frame(x, y, w, h, ['┌', '┐', '└', '┘', '─', '│'], Style::new(AMBER, PANEL));
        // title strip
        let title = format!(" ◢ {} ", self.title);
        t.text(x + 2, y + 1, &title, Style::new(OBSIDIAN, AMBER).bold(), w - 4);
        let host_w = self.host.chars().count();
        if title.chars().count() + host_w + 6 < w {
            t.text(x + w - 2 - host_w, y + 1, &self.host, Style::new(CYAN, PANEL), host_w);
        }
        let lx = x + 3;
        let label_w = 10;
        let fx = lx + label_w;
        let fw = w.saturating_sub(label_w + 6);
        let field_st = Style::new(TEXT, OBSIDIAN);
        let label = |f: Field| if self.field == f { Style::new(AMBER, PANEL).bold() } else { Style::new(DIM, PANEL) };
        t.text(lx, y + 3, "login", label(Field::User), label_w);
        self.user.draw(t, fx, y + 3, fw, field_st, self.field == Field::User, None);
        let pl = if self.prompt.is_empty() { "password".to_string() } else { self.prompt.to_lowercase() };
        t.text(lx, y + 5, &pl, label(Field::Pass), label_w - 1);
        self.pass.draw(t, fx, y + 5, fw, field_st, self.field == Field::Pass, Some('*'));
        if !self.warning.is_empty() {
            t.text(lx, y + 7, &self.warning, Style::new(CRIMSON, PANEL), w - 6);
        }
        if !self.msg.0.is_empty() {
            let st = Style::new(if self.msg.1 { CRIMSON } else { CYAN }, PANEL).bold();
            t.text(lx, y + 9, &self.msg.0, st, w - 6);
        } else if update_busy() {
            t.text(lx, y + 9, UPDATE_MSG, Style::new(AMBER, PANEL).bold(), w - 6);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let c = conf();
    let session = load_session(conf_get(&c, "session").as_deref());
    if args.iter().any(|a| a == "--self-test") {
        println!("session : {}", session.as_ref().map(|s| format!("{} ({})", s.id, s.cmd)).unwrap_or_else(|| "NONE — install shadow-tui-restricted".into()));
        match panel() {
            Some((n, px, mm)) => {
                let sc = scale::ideal_scale(&n, px, mm);
                println!("panel   : {n} {}x{} {}x{} mm -> scale {} -> font {}", px.0, px.1, mm.0, mm.1, sc.map(|s| s.to_string()).unwrap_or("?".into()), font_for(sc.unwrap_or(1.0)));
            }
            None => println!("panel   : not found (font left as is)"),
        }
        println!("font    : {}", conf_get(&c, "console_font").unwrap_or_else(|| "auto".into()));
        std::process::exit(if session.is_some() { 0 } else { 1 });
    }
    let font = set_console_font(&c);
    if args.iter().any(|a| a == "--font-only") {
        println!("{}", font.unwrap_or_else(|| "unchanged".into()));
        return;
    }
    let demo = args.iter().any(|a| a == "--demo");
    if !demo && std::env::var_os("GREETD_SOCK").is_none() {
        eprintln!("shadow-greet: GREETD_SOCK is not set (run under greetd, or use --demo)");
        std::process::exit(2);
    }
    let mut g = Greeter {
        title: conf_get(&c, "title").unwrap_or_else(|| "SHADOW TUI // RESTRICTED".into()),
        warning: conf_get(&c, "warning").unwrap_or_else(|| "AUTHORIZED ACCESS ONLY".into()),
        host: hostname(),
        session,
        user: Input::with(&last_user()),
        pass: Input::default(),
        field: Field::User,
        prompt: String::new(),
        msg: (String::new(), false),
        conn: None,
        awaiting: false,
        failures: 0,
        demo,
    };
    if !g.user.buf.is_empty() {
        g.field = Field::Pass;
    }
    let palette = Term::linux_console().then_some(VT_PALETTE);
    let mut t = match Term::enter(palette) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("shadow-greet: {e}");
            std::process::exit(2);
        }
    };
    let code = loop {
        t.resize();
        g.draw(&mut t);
        let _ = t.present();
        let mut out = None;
        for k in t.keys() {
            if let Some(c) = g.key(k) {
                out = Some(c);
                break;
            }
        }
        if let Some(c) = out {
            break c;
        }
    };
    t.leave();
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_follows_density() {
        assert_eq!(font_for(1.0), "ter-v16n");
        assert_eq!(font_for(1.5), "ter-v24n"); // Dell 14 Plus 2560x1600
        assert_eq!(font_for(2.0), "ter-v32n");
        assert_eq!(font_for(1.25), "ter-v20n");
    }

    #[test]
    fn update_blocks_login() {
        let m = std::env::temp_dir().join(format!("greet-mark-{}", std::process::id()));
        std::env::set_var("SHADOW_UPDATE_MARK", &m);
        let mut g = Greeter { title: String::new(), warning: String::new(), host: String::new(), session: None, user: Input::with("nami"), pass: Input::default(), field: Field::Pass, prompt: String::new(), msg: (String::new(), false), conn: None, awaiting: false, failures: 0, demo: true };
        g.pass.set("shadow");
        assert_eq!(g.submit(), None);
        assert!(g.msg.0.starts_with("demo: would start"));
        fs::write(&m, "kind=files\n").unwrap();
        g.pass.set("shadow");
        assert_eq!(g.submit(), None);
        assert_eq!(g.msg.0, UPDATE_MSG);
        assert!(g.pass.buf.is_empty(), "password is not kept");
        let _ = fs::remove_file(&m);
    }

    #[test]
    fn picks_shadow_session() {
        let d = std::env::temp_dir().join(format!("sess-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("niri.desktop"), "[Desktop Entry]\nName=Niri\nExec=niri-session\n").unwrap();
        fs::write(d.join("shadow-tui.desktop"), "[Desktop Entry]\nName=Shadow TUI Restricted\nExec=/opt/shadow-tui/bin/shadow-session\nDesktopNames=niri;shadow\n").unwrap();
        std::env::set_var("SHADOW_SESSIONS_DIR", &d);
        let s = load_session(None).unwrap();
        assert_eq!(s.id, "shadow-tui");
        assert!(s.env.contains(&"XDG_CURRENT_DESKTOP=niri:shadow".to_string()));
        assert_eq!(load_session(Some("niri")).unwrap().id, "niri");
    }

    #[test]
    fn edid_panel_size() {
        let r = std::env::temp_dir().join(format!("drm-{}", std::process::id()));
        let c = r.join("sys/class/drm/card1-eDP-1");
        fs::create_dir_all(&c).unwrap();
        fs::write(c.join("status"), "connected\n").unwrap();
        fs::write(c.join("modes"), "2560x1600\n1920x1200\n").unwrap();
        let mut edid = vec![0u8; 128];
        edid[21] = 30;
        edid[22] = 19;
        // DTD 1 at 54: horizontal 301 mm, vertical 188 mm
        edid[54 + 12] = (301 & 0xFF) as u8;
        edid[54 + 13] = (188 & 0xFF) as u8;
        edid[54 + 14] = (((301 >> 8) << 4) | (188 >> 8)) as u8;
        fs::write(c.join("edid"), &edid).unwrap();
        std::env::set_var("SHADOW_SYS_ROOT", &r);
        let (n, px, mm) = panel().unwrap();
        assert_eq!((n.as_str(), px, mm), ("eDP-1", (2560, 1600), (301, 188)));
        assert_eq!(font_for(scale::ideal_scale(&n, px, mm).unwrap()), "ter-v24n");
    }
}
