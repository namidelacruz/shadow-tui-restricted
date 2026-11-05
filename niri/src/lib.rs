//! The client side of shadow-compositor's control socket (10.0: Shadow's only compositor).
//!
//! The crate keeps its old name: the compositor answers in niri's IPC shapes (`niri msg --json`:
//! outputs, windows, workspaces, actions, the event stream), so every Shadow program that was
//! written against niri works unchanged. Nothing here runs a `niri` binary any more.

use serde_json::Value;

/// The running shadow-compositor's control socket: $SHADOW_SOCKET (exported by the compositor to
/// everything it starts), else the newest $XDG_RUNTIME_DIR/shadow-compositor.*.sock (for programs
/// started outside it, e.g. by systemd user units).
pub fn shadow_compositor() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("SHADOW_SOCKET").filter(|v| !v.is_empty()).map(std::path::PathBuf::from) {
        return p.exists().then_some(p);
    }
    let dir = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    let mut v: Vec<_> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with("shadow-compositor.") && n.ends_with(".sock")
        }))
        .collect();
    v.sort();
    v.pop()
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

/// A command for Shadow's own compositor (config-reload, state, action …), when it runs this session.
pub fn compositor_command(line: &str) -> Result<String, String> {
    match shadow_compositor() {
        Some(sock) => compositor_ask(&sock, line),
        None => Err("not running under shadow-compositor".into()),
    }
}

/// The session's compositor answers
pub fn session_alive() -> bool {
    shadow_compositor().is_some_and(|sock| compositor_ask(&sock, "ping").is_ok())
}

/// What `niri msg <args>` would answer, from shadow-compositor; Ok(stdout) on success.
pub fn msg(args: &[&str]) -> Result<String, String> {
    match shadow_compositor() {
        Some(sock) => compositor_ask(&sock, &format!("niri {}", args.join(" "))),
        None => Err("shadow-compositor is not running".into()),
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
    shadow_compositor().is_some()
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

/// The event stream in niri's format (`niri msg --json event-stream`); `f` returning false stops it.
pub fn event_stream(f: impl FnMut(&str) -> bool) {
    compositor_event_stream(f)
}

/// Shadow's own compositor has no push stream: ask it twice a second and turn the differences
/// into the same events niri sends (so the side bar and shadow-sessiond work unchanged).
fn compositor_event_stream(mut f: impl FnMut(&str) -> bool) {
    let mut last_ws = String::new();
    let mut last: Vec<Value> = vec![];
    let mut sent_initial = false;
    loop {
        let ok = (|| -> Option<()> {
            let ws = json(&["workspaces"]).ok()?;
            let wins = json(&["windows"]).ok()?;
            let wins = wins.as_array().cloned().unwrap_or_default();
            let ws_s = ws.to_string();
            let mut lines = vec![];
            if ws_s != last_ws {
                lines.push(serde_json::json!({ "WorkspacesChanged": { "workspaces": ws } }).to_string());
                last_ws = ws_s;
            }
            if !sent_initial {
                lines.push(serde_json::json!({ "WindowsChanged": { "windows": wins } }).to_string());
                sent_initial = true;
            } else {
                lines.extend(window_diff(&last, &wins));
            }
            last = wins;
            for l in lines {
                if !f(&l) {
                    return None;
                }
            }
            Some(())
        })();
        if ok.is_none() && (shadow_compositor().is_none() || !f("")) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// niri events for the change from `old` to `new` window lists
pub fn window_diff(old: &[Value], new: &[Value]) -> Vec<String> {
    let id = |v: &Value| v.get("id").and_then(|x| x.as_u64());
    let mut out = vec![];
    for w in new {
        let prev = old.iter().find(|o| id(o) == id(w));
        if prev != Some(w) {
            out.push(serde_json::json!({ "WindowOpenedOrChanged": { "window": w } }).to_string());
        }
    }
    for o in old {
        if !new.iter().any(|w| id(w) == id(o)) {
            out.push(serde_json::json!({ "WindowClosed": { "id": id(o) } }).to_string());
        }
    }
    let focus = |l: &[Value]| l.iter().find(|w| w.get("is_focused").and_then(|b| b.as_bool()).unwrap_or(false)).and_then(id);
    if focus(old) != focus(new) {
        out.push(serde_json::json!({ "WindowFocusChanged": { "id": focus(new) } }).to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compositor_polling_becomes_niri_events() {
        let a: Vec<Value> = serde_json::from_str(r#"[{"id":1,"app_id":"foot","title":"a","pid":5,"workspace_id":1,"is_focused":true}]"#).unwrap();
        let b: Vec<Value> = serde_json::from_str(r#"[{"id":1,"app_id":"foot","title":"a","pid":5,"workspace_id":1,"is_focused":false},{"id":2,"app_id":"shadow-files","title":"f","pid":6,"workspace_id":1,"is_focused":true}]"#).unwrap();
        let mut m = Model::default();
        m.apply(&serde_json::json!({ "WindowsChanged": { "windows": a } }).to_string());
        let mut changes = vec![];
        for l in window_diff(&a, &b) {
            changes.extend(m.apply(&l));
        }
        assert!(changes.contains(&Change::Opened { app_id: "shadow-files".into(), title: "f".into(), id: 2 }));
        assert_eq!(m.focused().map(|w| w.id), Some(2));
        let mut changes = vec![];
        for l in window_diff(&b, &a[..0]) {
            changes.extend(m.apply(&l));
        }
        assert!(changes.contains(&Change::Closed { id: 1 }) && changes.contains(&Change::Closed { id: 2 }));
    }

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
