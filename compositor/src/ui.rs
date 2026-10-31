//! Login screen (greeter mode, run by greetd) and lock screen — both drawn by the compositor itself
//! with the same backdrop shader, so the greeter's last frame (globe collapsed into the floor grid)
//! is the session's first frame: the login blends into the desktop instead of cutting to it.
use smithay::input::keyboard::Keysym;
use std::time::{Duration, Instant};

#[derive(PartialEq, Clone, Copy, Debug)]
pub enum Mode {
    Greeter,
    Lock,
}

pub enum UiMsg {
    /// greeter: session started by greetd / lock: PAM accepted
    Success,
    Fail(String),
}

#[derive(Debug, PartialEq)]
pub enum UiAction {
    None,
    Submit,
    Rescue,
    Reboot,
    Poweroff,
}

pub struct Ui {
    pub mode: Mode,
    pub user: String,
    pub pass: String,
    pub field: usize,
    pub sessions: Vec<(String, String)>,
    pub sess: usize,
    pub status: (String, u8),
    pub busy: bool,
    pub rot: f32,
    last_tick: Instant,
    active_until: Instant,
    pub handoff: Option<Instant>,
    pub warning: Option<String>,
    confirm: Option<(Keysym, Instant)>,
    pub host: String,
}

pub const HANDOFF: Duration = Duration::from_millis(450);

fn conf(key: &str) -> Option<String> {
    let p = std::env::var("SHADOW_GREETER_CONF").unwrap_or_else(|_| "/etc/shadow-tui/greeter.conf".into());
    std::fs::read_to_string(p).ok()?.lines().find_map(|l| l.strip_prefix(&format!("{key}=")).map(|v| v.to_string()))
}

pub fn sessions() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = vec![];
    let dir = std::env::var("SHADOW_SESSIONS_DIR").unwrap_or_else(|_| "/usr/share/wayland-sessions".into());
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().map_or(false, |e| e == "desktop")).collect();
        files.sort();
        for f in files {
            let t = std::fs::read_to_string(&f).unwrap_or_default();
            let get = |k: &str| t.lines().find_map(|l| l.strip_prefix(&format!("{k}=")).map(String::from));
            if let (Some(n), Some(e)) = (get("Name"), get("Exec")) {
                v.push((n, e));
            }
        }
    }
    // our own session first
    v.sort_by_key(|(n, _)| !n.contains("Shadow"));
    if v.is_empty() {
        v.push(("Shadow TUI Restricted".into(), "shadow-session".into()));
    }
    v
}

impl Ui {
    pub fn new(mode: Mode) -> Ui {
        let host = std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "localhost".into());
        let user = match mode {
            Mode::Lock => std::env::var("USER").unwrap_or_default(),
            Mode::Greeter => conf("default_user").or_else(|| std::fs::read_to_string("/var/cache/shadow-tui-greeter/last-user").ok().map(|s| s.trim().to_string())).unwrap_or_default(),
        };
        let sessions = sessions();
        let want = conf("default_session").unwrap_or_default();
        let sess = sessions.iter().position(|(n, _)| !want.is_empty() && n.to_lowercase().contains(&want.to_lowercase())).unwrap_or(0);
        let warning = match conf("warning") {
            Some(w) if w.trim().is_empty() => None,
            Some(w) => Some(w),
            None => Some("AUTHORIZED ACCESS ONLY · activity on this system is monitored and logged".into()),
        };
        Ui {
            field: if mode == Mode::Lock || !user.is_empty() { 1 } else { 0 },
            mode, user, pass: String::new(), sessions, sess, status: (String::new(), 0), busy: false, rot: 0.0,
            last_tick: Instant::now(), active_until: Instant::now() + Duration::from_secs(8), handoff: None, warning, confirm: None, host,
        }
    }

    pub fn globe(&self) -> f32 {
        match self.handoff {
            Some(t) => (1.0 - t.elapsed().as_secs_f32() / HANDOFF.as_secs_f32()).max(0.0).powf(1.6),
            None => 1.0,
        }
    }
    pub fn text_alpha(&self) -> f32 {
        match self.handoff {
            Some(t) => (1.0 - t.elapsed().as_secs_f32() / (HANDOFF.as_secs_f32() * 0.6)).max(0.0),
            None => 1.0,
        }
    }
    /// the globe spins for a few seconds after input, then holds still (no redraws while idle)
    pub fn moving(&self) -> bool {
        self.handoff.is_some() || self.busy || Instant::now() < self.active_until
    }
    pub fn tick(&mut self) {
        let dt = self.last_tick.elapsed().as_secs_f32().min(0.1);
        self.last_tick = Instant::now();
        if self.moving() {
            self.rot = (self.rot + dt * 0.6) % std::f32::consts::TAU;
        }
    }
    pub fn handoff_done(&self) -> bool {
        self.handoff.map(|t| t.elapsed() >= HANDOFF).unwrap_or(false)
    }

    pub fn lines(&self) -> Vec<(String, u8)> {
        let mut v = vec![("S H A D O W   T U I   //   R E S T R I C T E D".to_string(), 1u8)];
        match self.mode {
            Mode::Greeter => v.push((self.warning.clone().unwrap_or_default(), 2)),
            Mode::Lock => v.push((format!("SESSION LOCKED · {}", self.host.to_uppercase()), 2)),
        }
        let caret = |on: bool| if on { "_" } else { "" };
        if self.mode == Mode::Greeter {
            v.push((format!("{} USER  {}{}", if self.field == 0 { "▶" } else { " " }, self.user, caret(self.field == 0)), if self.field == 0 { 1 } else { 0 }));
        }
        v.push((format!("{} PASS  {}{}", if self.field == 1 { "▶" } else { " " }, "•".repeat(self.pass.chars().count()), caret(self.field == 1)), if self.field == 1 { 1 } else { 0 }));
        if self.mode == Mode::Greeter {
            let s = self.sessions.get(self.sess).map(|s| s.0.clone()).unwrap_or_default();
            v.push((format!("◂ {s} ▸"), 3));
        }
        let st = if self.busy { ("AUTHENTICATING…".to_string(), 3) } else { self.status.clone() };
        v.push((st.0, st.1.max(if st.1 == 0 { 0 } else { st.1 })));
        v.push((match self.mode {
            Mode::Greeter => "Enter sign in · Tab field · ←→ session · F2 rescue shell · F11/F12 reboot/power off (press twice)".into(),
            Mode::Lock => "Enter unlock".into(),
        }, 0));
        v
    }

    pub fn key(&mut self, sym: Keysym, ch: Option<char>) -> UiAction {
        self.active_until = Instant::now() + Duration::from_secs(6);
        if self.busy || self.handoff.is_some() {
            return UiAction::None;
        }
        let greeter = self.mode == Mode::Greeter;
        match sym {
            Keysym::Return | Keysym::KP_Enter => {
                if greeter && self.field == 0 {
                    self.field = 1;
                    return UiAction::None;
                }
                if (greeter && self.user.is_empty()) || self.pass.is_empty() && self.mode == Mode::Lock {
                    self.status = ("enter your credentials".into(), 2);
                    return UiAction::None;
                }
                return UiAction::Submit;
            }
            Keysym::Tab | Keysym::Down | Keysym::Up if greeter => self.field = 1 - self.field,
            Keysym::Left if greeter && !self.sessions.is_empty() => self.sess = (self.sess + self.sessions.len() - 1) % self.sessions.len(),
            Keysym::Right if greeter && !self.sessions.is_empty() => self.sess = (self.sess + 1) % self.sessions.len(),
            Keysym::BackSpace => {
                if self.field == 0 { self.user.pop(); } else { self.pass.pop(); }
            }
            Keysym::Escape => {
                self.pass.clear();
                self.status = (String::new(), 0);
            }
            Keysym::F2 if greeter => return UiAction::Rescue,
            Keysym::F11 | Keysym::F12 if greeter => {
                if let Some((k, t)) = self.confirm {
                    if k == sym && t.elapsed() < Duration::from_secs(3) {
                        return if sym == Keysym::F11 { UiAction::Reboot } else { UiAction::Poweroff };
                    }
                }
                self.confirm = Some((sym, Instant::now()));
                self.status = (format!("press {} again to {}", if sym == Keysym::F11 { "F11" } else { "F12" }, if sym == Keysym::F11 { "reboot" } else { "power off" }), 2);
            }
            _ => {
                if let Some(c) = ch.filter(|c| !c.is_control()) {
                    if self.field == 0 && greeter { self.user.push(c) } else if self.pass.len() < 512 { self.pass.push(c) }
                }
            }
        }
        UiAction::None
    }
}

// ---------------------------------------------------------------- greetd client (greeter mode)
fn greetd_rt(s: &mut std::os::unix::net::UnixStream, msg: &serde_json::Value) -> Result<serde_json::Value, String> {
    use std::io::{Read, Write};
    let body = msg.to_string();
    s.write_all(&(body.len() as u32).to_ne_bytes()).and_then(|_| s.write_all(body.as_bytes())).map_err(|e| e.to_string())?;
    let mut len = [0u8; 4];
    s.read_exact(&mut len).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; u32::from_ne_bytes(len) as usize];
    s.read_exact(&mut buf).map_err(|e| e.to_string())?;
    serde_json::from_slice(&buf).map_err(|e| e.to_string())
}

/// Log in through greetd and ask it to start `cmd`. Demo mode (SHADOW_GREET_DEMO=1) accepts the
/// password "shadow" without greetd, for previews and tests.
pub fn greetd_login(user: &str, pass: &str, cmd: &str) -> Result<(), String> {
    use serde_json::json;
    if std::env::var("SHADOW_GREET_DEMO").is_ok() {
        std::thread::sleep(Duration::from_millis(400));
        return if pass == "shadow" { Ok(()) } else { Err("authentication failed (demo password: shadow)".into()) };
    }
    let sock = std::env::var("GREETD_SOCK").map_err(|_| "GREETD_SOCK not set (not started by greetd)".to_string())?;
    let mut s = std::os::unix::net::UnixStream::connect(sock).map_err(|e| e.to_string())?;
    let mut r = greetd_rt(&mut s, &json!({"type": "create_session", "username": user}))?;
    loop {
        match r["type"].as_str() {
            Some("auth_message") => {
                let resp = match r["auth_message_type"].as_str() {
                    Some("secret") | Some("visible") => json!(pass),
                    _ => serde_json::Value::Null,
                };
                r = greetd_rt(&mut s, &json!({"type": "post_auth_message_response", "response": resp}))?;
            }
            Some("success") => break,
            _ => {
                let _ = greetd_rt(&mut s, &json!({"type": "cancel_session"}));
                return Err(r["description"].as_str().unwrap_or("authentication failed").to_string());
            }
        }
    }
    let argv: Vec<String> = cmd.split_whitespace().map(String::from).collect();
    let r = greetd_rt(&mut s, &json!({"type": "start_session", "cmd": argv, "env": []}))?;
    if r["type"] == "success" {
        let _ = std::fs::create_dir_all("/var/cache/shadow-tui-greeter");
        let _ = std::fs::write("/var/cache/shadow-tui-greeter/last-user", user);
        Ok(())
    } else {
        let _ = greetd_rt(&mut s, &json!({"type": "cancel_session"}));
        Err(r["description"].as_str().unwrap_or("could not start the session").to_string())
    }
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn keys() {
        let mut u = Ui::new(Mode::Greeter);
        u.user.clear();
        u.field = 0;
        for c in "jane".chars() {
            u.key(Keysym::a, Some(c));
        }
        assert_eq!(u.key(Keysym::Return, None), UiAction::None); // moves to the password field
        u.key(Keysym::a, Some('x'));
        u.key(Keysym::BackSpace, None);
        for c in "pw".chars() {
            u.key(Keysym::a, Some(c));
        }
        assert_eq!((u.user.as_str(), u.pass.as_str()), ("jane", "pw"));
        assert_eq!(u.key(Keysym::Return, None), UiAction::Submit);
        assert_eq!(u.key(Keysym::F12, None), UiAction::None);
        assert_eq!(u.key(Keysym::F12, None), UiAction::Poweroff);
        assert!(u.lines().iter().any(|(l, _)| l.contains("••")));
        let mut l = Ui::new(Mode::Lock);
        assert_eq!(l.key(Keysym::Return, None), UiAction::None);
        l.key(Keysym::a, Some('s'));
        assert_eq!(l.key(Keysym::Return, None), UiAction::Submit);
    }
}
