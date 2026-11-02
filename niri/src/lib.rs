//! Talks to niri only through its documented public interfaces: `niri msg --json` (IPC) and the
//! KDL config file. Nothing here depends on niri's source; field access is tolerant so a newer
//! niri that adds fields keeps working.
pub mod config;

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

fn niri() -> Command {
    let mut c = Command::new(std::env::var("SHADOW_NIRI").unwrap_or_else(|_| "niri".into()));
    c.stdin(Stdio::null());
    c
}

/// Which compositor this session runs: niri, or Shadow's own (shadow-compositor). Shadow's own
/// exports SHADOW_SOCKET and answers the same questions in niri's format (`niri …` on its socket).
pub fn shadow_compositor() -> Option<std::path::PathBuf> {
    if std::env::var_os("NIRI_SOCKET").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    std::env::var_os("SHADOW_SOCKET").filter(|v| !v.is_empty()).map(std::path::PathBuf::from).filter(|p| p.exists())
}

fn compositor_ask(sock: &std::path::Path, line: &str) -> Result<String, String> {
    use std::io::{Read, Write};
    let mut s = std::os::unix::net::UnixStream::connect(sock).map_err(|e| format!("shadow-compositor: {e}"))?;
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
    s.write_all(format!("{line}\n").as_bytes()).map_err(|e| e.to_string())?;
    let mut out = String::new();
    s.read_to_string(&mut out).map_err(|e| e.to_string())?;
    let out = out.trim_end().to_string();
    match out.strip_prefix("error: ") {
        Some(e) => Err(e.to_string()),
        None => Ok(out),
    }
}

/// The running session answers (niri, or Shadow's own compositor)
pub fn session_alive() -> bool {
    match shadow_compositor() {
        Some(sock) => compositor_ask(&sock, "ping").is_ok(),
        None => std::env::var_os("NIRI_SOCKET").is_some_and(|p| std::path::Path::new(&p).exists()) && msg(&["version"]).is_ok(),
    }
}

/// `niri msg <args>`; Ok(stdout) on success.
pub fn msg(args: &[&str]) -> Result<String, String> {
    if let Some(sock) = shadow_compositor() {
        return compositor_ask(&sock, &format!("niri {}", args.join(" ")));
    }
    let out = niri().arg("msg").args(args).output().map_err(|e| format!("niri: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// `niri msg --json <args>` parsed.
pub fn json(args: &[&str]) -> Result<Value, String> {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    serde_json::from_str(&msg(&a)?).map_err(|e| format!("niri json: {e}"))
}

/// `niri msg action <args>`
pub fn action(args: &[&str]) -> Result<(), String> {
    let mut a = vec!["action"];
    a.extend_from_slice(args);
    msg(&a).map(|_| ())
}

pub fn available() -> bool {
    std::env::var_os("NIRI_SOCKET").is_some() || shadow_compositor().is_some()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mode {
    pub w: i32,
    pub h: i32,
    pub mhz: i32,
    pub preferred: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Output {
    pub name: String,
    pub make: String,
    pub model: String,
    pub mm: Option<(i32, i32)>,
    pub modes: Vec<Mode>,
    pub current: Option<usize>,
    /// None when the output is disabled
    pub scale: Option<f64>,
    pub logical: Option<(i32, i32, i32, i32)>,
}

impl Output {
    pub fn mode(&self) -> Option<&Mode> {
        self.current.and_then(|i| self.modes.get(i))
    }
    pub fn enabled(&self) -> bool {
        self.logical.is_some()
    }
    pub fn internal(&self) -> bool {
        if std::env::var("SHADOW_FAKE_INTERNAL").is_ok_and(|v| v == self.name) {
            return true;
        }
        let c = self.name.to_ascii_uppercase();
        c.starts_with("EDP") || c.starts_with("LVDS") || c.starts_with("DSI")
    }
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}
fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
}

pub fn parse_output(name: &str, o: &Value) -> Output {
    let modes = o.get("modes").and_then(|m| m.as_array()).map(|a| a.iter().map(|m| Mode { w: i(m, "width") as i32, h: i(m, "height") as i32, mhz: i(m, "refresh_rate") as i32, preferred: b(m, "is_preferred") }).collect()).unwrap_or_default();
    let mm = o.get("physical_size").and_then(|p| p.as_array()).filter(|a| a.len() == 2).map(|a| (a[0].as_i64().unwrap_or(0) as i32, a[1].as_i64().unwrap_or(0) as i32));
    let lg = o.get("logical").filter(|l| !l.is_null());
    Output {
        name: if name.is_empty() { s(o, "name") } else { name.to_string() },
        make: s(o, "make"),
        model: s(o, "model"),
        mm,
        modes,
        current: o.get("current_mode").and_then(|c| c.as_u64()).map(|c| c as usize),
        scale: lg.and_then(|l| l.get("scale")).and_then(|x| x.as_f64()),
        logical: lg.map(|l| (i(l, "x") as i32, i(l, "y") as i32, i(l, "width") as i32, i(l, "height") as i32)),
    }
}

pub fn outputs() -> Result<Vec<Output>, String> {
    let v = json(&["outputs"])?;
    let mut out: Vec<Output> = v.as_object().map(|m| m.iter().map(|(k, o)| parse_output(k, o)).collect()).unwrap_or_default();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

pub fn focused_output() -> Option<String> {
    json(&["focused-output"]).ok().map(|v| s(&v, "name")).filter(|n| !n.is_empty())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Workspace {
    pub id: u64,
    pub idx: u64,
    pub name: Option<String>,
    pub output: String,
    pub active: bool,
    pub focused: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Window {
    pub id: u64,
    pub title: String,
    pub app_id: String,
    pub pid: i64,
    pub workspace: u64,
    pub focused: bool,
}

fn parse_ws(v: &Value) -> Workspace {
    Workspace { id: i(v, "id") as u64, idx: i(v, "idx") as u64, name: v.get("name").and_then(|n| n.as_str()).map(String::from), output: s(v, "output"), active: b(v, "is_active"), focused: b(v, "is_focused") }
}
fn parse_win(v: &Value) -> Window {
    Window { id: i(v, "id") as u64, title: s(v, "title"), app_id: s(v, "app_id"), pid: i(v, "pid"), workspace: i(v, "workspace_id") as u64, focused: b(v, "is_focused") }
}

/// What happened, in terms the Shadow daemons care about.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    Focus { app_id: String, title: String, pid: i64 },
    Opened { app_id: String, title: String, id: u64 },
    Closed { id: u64 },
    Workspace,
    Output,
    Config { ok: bool },
}

/// Live model of niri's state, fed by the event stream.
#[derive(Default, Clone, Debug)]
pub struct Model {
    pub workspaces: Vec<Workspace>,
    pub windows: Vec<Window>,
    pub focused_window: Option<u64>,
    pub config_ok: bool,
}

impl Model {
    pub fn focused_output(&self) -> Option<&str> {
        self.workspaces.iter().find(|w| w.focused).map(|w| w.output.as_str())
    }
    pub fn focused(&self) -> Option<&Window> {
        self.focused_window.and_then(|id| self.windows.iter().find(|w| w.id == id))
    }
    pub fn active_ws(&self, output: &str) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.output == output && w.active)
    }
    pub fn windows_on(&self, ws: u64) -> usize {
        self.windows.iter().filter(|w| w.workspace == ws).count()
    }

    /// Apply one line of `niri msg --json event-stream`.
    pub fn apply(&mut self, line: &str) -> Vec<Change> {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return vec![] };
        let Some((kind, body)) = v.as_object().and_then(|o| o.iter().next()) else { return vec![] };
        let before_focus = self.focused_window;
        let mut out = vec![];
        match kind.as_str() {
            "WorkspacesChanged" => {
                self.workspaces = body.get("workspaces").and_then(|a| a.as_array()).map(|a| a.iter().map(parse_ws).collect()).unwrap_or_default();
                out.push(Change::Workspace);
            }
            "WorkspaceActivated" => {
                let id = i(body, "id") as u64;
                let focused = b(body, "focused");
                let output = self.workspaces.iter().find(|w| w.id == id).map(|w| w.output.clone()).unwrap_or_default();
                for w in &mut self.workspaces {
                    if w.output == output {
                        w.active = w.id == id;
                    }
                    if focused {
                        w.focused = w.id == id;
                    }
                }
                out.push(Change::Workspace);
            }
            "WindowsChanged" => {
                self.windows = body.get("windows").and_then(|a| a.as_array()).map(|a| a.iter().map(parse_win).collect()).unwrap_or_default();
                self.focused_window = self.windows.iter().find(|w| w.focused).map(|w| w.id);
            }
            "WindowOpenedOrChanged" => {
                if let Some(w) = body.get("window").map(parse_win) {
                    let new = !self.windows.iter().any(|x| x.id == w.id);
                    if w.focused {
                        self.focused_window = Some(w.id);
                        for x in &mut self.windows {
                            x.focused = false;
                        }
                    }
                    if new {
                        out.push(Change::Opened { app_id: w.app_id.clone(), title: w.title.clone(), id: w.id });
                    }
                    match self.windows.iter_mut().find(|x| x.id == w.id) {
                        Some(x) => *x = w,
                        None => self.windows.push(w),
                    }
                }
            }
            "WindowClosed" => {
                let id = i(body, "id") as u64;
                self.windows.retain(|w| w.id != id);
                if self.focused_window == Some(id) {
                    self.focused_window = None;
                }
                out.push(Change::Closed { id });
            }
            "WindowFocusChanged" => {
                self.focused_window = body.get("id").and_then(|x| x.as_u64());
                for w in &mut self.windows {
                    w.focused = Some(w.id) == self.focused_window;
                }
            }
            "ConfigLoaded" => {
                self.config_ok = !b(body, "failed");
                out.push(Change::Config { ok: self.config_ok });
            }
            _ => {}
        }
        if self.focused_window != before_focus || out.iter().any(|c| matches!(c, Change::Opened { .. })) {
            if let Some(w) = self.focused() {
                out.push(Change::Focus { app_id: w.app_id.clone(), title: w.title.clone(), pid: w.pid });
            }
        }
        out
    }
}

/// `niri msg --json event-stream`, line by line. Reconnects with a short back-off if niri
/// restarts; `f` returning false stops the stream.
pub fn event_stream(mut f: impl FnMut(&str) -> bool) {
    loop {
        let child: Option<Child> = niri().args(["msg", "--json", "event-stream"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok();
        if let Some(mut c) = child {
            if let Some(out) = c.stdout.take() {
                for line in BufReader::new(out).lines() {
                    let Ok(line) = line else { break };
                    if !f(&line) {
                        let _ = c.kill();
                        return;
                    }
                }
            }
            let _ = c.kill();
            let _ = c.wait();
        }
        if !f("") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_follows_events() {
        let mut m = Model::default();
        m.apply(r#"{"WorkspacesChanged":{"workspaces":[{"id":1,"idx":1,"name":null,"output":"eDP-1","is_active":true,"is_focused":true,"active_window_id":null},{"id":3,"idx":1,"name":null,"output":"DP-1","is_active":true,"is_focused":false,"active_window_id":null}]}}"#);
        assert_eq!(m.focused_output(), Some("eDP-1"));
        let c = m.apply(r#"{"WindowOpenedOrChanged":{"window":{"id":7,"title":"t","app_id":"shadow-term","pid":42,"workspace_id":1,"is_focused":true}}}"#);
        assert!(c.contains(&Change::Opened { app_id: "shadow-term".into(), title: "t".into(), id: 7 }));
        assert!(c.iter().any(|x| matches!(x, Change::Focus { pid: 42, .. })));
        m.apply(r#"{"WorkspaceActivated":{"id":3,"focused":true}}"#);
        assert_eq!(m.focused_output(), Some("DP-1"));
        m.apply(r#"{"WindowClosed":{"id":7}}"#);
        assert!(m.focused().is_none());
        assert_eq!(m.apply(r#"{"ConfigLoaded":{"failed":true}}"#), vec![Change::Config { ok: false }]);
    }

    #[test]
    fn parses_outputs() {
        let v: Value = serde_json::from_str(r#"{"name":"eDP-1","make":"BOE","model":"X","physical_size":[301,188],"modes":[{"width":2560,"height":1600,"refresh_rate":120000,"is_preferred":true},{"width":2560,"height":1600,"refresh_rate":60000,"is_preferred":false}],"current_mode":0,"logical":{"x":0,"y":0,"width":1707,"height":1067,"scale":1.5,"transform":"Normal"}}"#).unwrap();
        let o = parse_output("eDP-1", &v);
        assert!(o.internal() && o.enabled());
        assert_eq!(o.mm, Some((301, 188)));
        assert_eq!(o.mode().unwrap().mhz, 120000);
        assert_eq!(o.scale, Some(1.5));
    }
}
