//! shadow-webd — the local API behind Shadow's settings web app (9.0). Settings, the fix center,
//! default apps, the raw config and the crash log are one installable web app (PWA) served from
//! this machine; this service is its only way to read or change anything.
//!
//!   shadow-webd            serve (started with the session); prints nothing, logs to stderr
//!   shadow-webd url [PAGE] the URL that unlocks the app for this session (used by shadow-settings)
//!
//! Safety:
//!   * binds 127.0.0.1 only, on a port fixed per user (47000 + uid % 1000) so the installed app
//!     keeps its origin;
//!   * a new secret key is generated at every start (32 random bytes) and written to
//!     $XDG_RUNTIME_DIR/shadow-web/key, readable by you alone. The app gets it once in the URL
//!     fragment (never sent over the network), trades it for an HttpOnly, SameSite=Strict cookie
//!     and removes it from the address bar;
//!   * every API call needs that cookie or the key; the Host header must be this address (no DNS
//!     rebinding), a cross-site Origin is refused, and writes must be JSON (no form posts);
//!   * nothing here runs as root: repairs that need the password open a terminal, where sudo asks,
//!     exactly as the fix center did.
use shadow_apps::cfg::{get_in, set_in};
use shadow_apps::{atomic_write, cmd_out, config_path, spawn};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE: &str = include_str!("../web/style.css");
const SW: &str = include_str!("../web/sw.js");
const MANIFEST: &str = include_str!("../web/manifest.webmanifest");
const ICON: &str = include_str!("../web/icon.svg");
const FONT: &[u8] = include_bytes!("../../../assets/fonts/ShadowGlyph-Regular.ttf");

pub fn port() -> u16 {
    // SAFETY: getuid never fails
    47000 + (unsafe { libc::getuid() } % 1000) as u16
}

fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir());
    base.join("shadow-web")
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// constant-time comparison for secrets
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct Shared {
    key: String,
    cookie: String,
}

struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}
impl Req {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(h, _)| h.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
    fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

fn read_req(s: &mut TcpStream) -> Option<Req> {
    s.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok()?;
    let mut r = BufReader::new(s.try_clone().ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = vec![];
    loop {
        let mut h = String::new();
        r.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if headers.len() > 64 {
            return None;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    if len > 2 * 1024 * 1024 {
        return None;
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).ok()?;
    Some(Req { method, path, headers, body })
}

fn respond(s: &mut TcpStream, code: u16, ctype: &str, body: &[u8], extra: &[(&str, String)]) {
    let reason = match code { 200 => "OK", 400 => "Bad Request", 401 => "Unauthorized", 403 => "Forbidden", 404 => "Not Found", _ => "Error" };
    let mut h = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: default-src 'self'; style-src 'self'; script-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'none'\r\n",
        body.len()
    );
    for (k, v) in extra {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    let _ = s.write_all(h.as_bytes());
    let _ = s.write_all(body);
}

fn json_ok(s: &mut TcpStream, v: serde_json::Value) {
    respond(s, 200, "application/json", v.to_string().as_bytes(), &[]);
}
fn json_err(s: &mut TcpStream, code: u16, msg: &str) {
    respond(s, code, "application/json", serde_json::json!({ "error": msg }).to_string().as_bytes(), &[]);
}

// ── the settings schema (the old settings matrix + 9.0's own keys) ─────────────────────────────

fn row(section: &str, key: &str, label: &str, kind: serde_json::Value, default: &str) -> serde_json::Value {
    serde_json::json!({ "section": section, "key": key, "label": label, "kind": kind, "default": default })
}
fn num(min: f64, max: f64, step: f64) -> serde_json::Value {
    serde_json::json!({ "type": "num", "min": min, "max": max, "step": step })
}
fn choice(c: &[&str]) -> serde_json::Value {
    serde_json::json!({ "type": "choice", "choices": c })
}
fn kind(t: &str) -> serde_json::Value {
    serde_json::json!({ "type": t })
}

fn schema() -> serde_json::Value {
    serde_json::json!([
        { "name": "DESKTOP", "about": "the app area, windows and their unfolding", "rows": [
            row("layout", "max_open", "Apps open at once (more minimise the oldest)", num(1.0, 9.0, 1.0), "4"),
            row("layout", "gap", "Space between tiles (px)", num(0.0, 48.0, 2.0), "14"),
            row("layout", "tile_px", "Minimised tile size (px)", num(64.0, 256.0, 8.0), "128"),
            row("layout", "unfold_ms", "Kinetic unfolding (ms, 0 = off)", num(0.0, 2000.0, 40.0), "560"),
            row("layout", "anim_ms", "Re-tiling time (ms)", num(0.0, 600.0, 20.0), "160"),
        ]},
        { "name": "LOOK", "about": "gold line work, the background light, text", "rows": [
            row("look", "motion", "Background light", choice(&["on", "low", "off"]), "on"),
            row("look", "amber", "Gold", kind("color"), "#D4AA5A"),
            row("look", "cyan", "Highlight gold", kind("color"), "#F0DB9C"),
            row("look", "crimson", "Alert", kind("color"), "#C84B31"),
            row("look", "obsidian", "Obsidian", kind("color"), "#0E0C09"),
            row("look", "font_pt", "Text size (pt)", num(8.0, 20.0, 0.5), "11"),
            row("look", "window_opacity", "Window opacity", num(0.4, 1.0, 0.02), "0.86"),
        ]},
        { "name": "INPUT", "about": "keyboard, mouse and touchpad", "rows": [
            row("input", "xkb_layout", "Keyboard layout (e.g. us,ca)", kind("text"), ""),
            row("input", "repeat_delay", "Key repeat delay (ms)", num(150.0, 800.0, 25.0), "300"),
            row("input", "repeat_rate", "Key repeat rate (/s)", num(10.0, 80.0, 5.0), "40"),
            row("input", "tap_to_click", "Tap to click", kind("bool"), "true"),
            row("input", "natural_scroll", "Natural scrolling", kind("bool"), "false"),
            row("input", "pointer_speed", "Pointer speed", num(-1.0, 1.0, 0.1), "0"),
            row("input", "pointer", "Mouse", choice(&["all", "apps"]), "all"),
        ]},
        { "name": "DISPLAYS", "about": "refresh rates on mains and battery", "rows": [
            row("display", "refresh_policy", "Refresh policy", choice(&["auto", "ac", "battery", "fixed"]), "auto"),
            row("display", "refresh_ac", "Refresh on mains (Hz)", num(48.0, 240.0, 12.0), "120"),
            row("display", "refresh_battery", "Refresh on battery (Hz)", num(30.0, 120.0, 12.0), "60"),
        ]},
        { "name": "SESSION", "about": "idle lock and power", "rows": [
            row("session", "lock_after_min", "Lock after idle (min, 0 = never)", num(0.0, 120.0, 1.0), "10"),
            row("session", "screen_off_after_min", "Monitors off after idle (min)", num(0.0, 180.0, 1.0), "12"),
            row("power", "learning", "Learn and predict (shadow-powerd)", kind("bool"), "true"),
        ]},
        { "name": "UPDATES", "about": "update source and policy", "rows": [
            row("updates", "repo", "Update source (git URL)", kind("text"), ""),
            row("updates", "branch", "Branch", kind("text"), "main"),
            row("updates", "require_private", "Require a private repository", kind("bool"), "true"),
            row("updates", "require_signed", "Require signed commits", kind("bool"), "false"),
        ]},
    ])
}

/// validate a value for a schema row and turn it into a TOML literal
fn literal(kind: &serde_json::Value, v: &serde_json::Value) -> Result<String, String> {
    let t = kind.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match t {
        "num" => {
            let x = v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).ok_or("not a number")?;
            let (lo, hi) = (kind["min"].as_f64().unwrap_or(f64::MIN), kind["max"].as_f64().unwrap_or(f64::MAX));
            if !(lo..=hi).contains(&x) {
                return Err(format!("must be {lo}–{hi}"));
            }
            let step = kind["step"].as_f64().unwrap_or(1.0);
            Ok(if step.fract() == 0.0 && lo.fract() == 0.0 { format!("{}", x.round() as i64) } else { format!("{x}") })
        }
        "bool" => Ok(v.as_bool().ok_or("not true / false")?.to_string()),
        "choice" => {
            let s = v.as_str().ok_or("not text")?;
            if !kind["choices"].as_array().is_some_and(|c| c.iter().any(|x| x.as_str() == Some(s))) {
                return Err("not one of the choices".into());
            }
            Ok(format!("\"{s}\""))
        }
        "color" => {
            let s = v.as_str().ok_or("not text")?;
            let ok = s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit());
            if !ok {
                return Err("colour must be #RRGGBB".into());
            }
            Ok(format!("\"{s}\""))
        }
        _ => {
            let s = v.as_str().ok_or("not text")?;
            if s.len() > 512 || s.contains('\n') {
                return Err("too long".into());
            }
            Ok(serde_json::Value::String(s.into()).to_string())
        }
    }
}

fn conf() -> String {
    std::fs::read_to_string(config_path()).unwrap_or_default()
}

/// write config.toml and apply: niri's file is regenerated and a running Shadow compositor reloads
fn apply(text: &str) -> Result<String, String> {
    let p = config_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::copy(&p, p.with_extension("toml.bak"));
    atomic_write(&p, text).map_err(|e| e.to_string())?;
    let (c, o) = cmd_out(&["shadow", "setup", "niri"]);
    let _ = shadow_niri::compositor_command("config-reload");
    Ok(if c == 0 { "applied".into() } else { format!("saved; niri: {}", o.lines().last().unwrap_or("")) })
}

fn settings_get() -> serde_json::Value {
    let text = conf();
    let mut s = schema();
    for page in s.as_array_mut().unwrap() {
        for r in page["rows"].as_array_mut().unwrap() {
            let v = get_in(&text, r["section"].as_str().unwrap(), r["key"].as_str().unwrap());
            r["value"] = v.map(|v| serde_json::Value::String(v.trim_matches('"').to_string())).unwrap_or(serde_json::Value::Null);
        }
    }
    s
}

fn settings_set(j: &serde_json::Value) -> Result<String, String> {
    let (sec, key) = (j["section"].as_str().ok_or("section")?, j["key"].as_str().ok_or("key")?);
    let s = schema();
    let row = s.as_array().unwrap().iter().flat_map(|p| p["rows"].as_array().unwrap().iter()).find(|r| r["section"] == sec && r["key"] == key).ok_or("unknown setting")?;
    let lit = literal(&row["kind"], &j["value"])?;
    apply(&set_in(&conf(), sec, key, &lit))
}

// ── fix center ─────────────────────────────────────────────────────────────────────────────────

fn fix_issues() -> serde_json::Value {
    let (_, o) = cmd_out(&["shadow", "fix", "scan", "--porcelain"]);
    let v: Vec<serde_json::Value> = o.lines().filter_map(|l| {
        let (head, detail) = l.split_once('|')?;
        let mut w = head.splitn(5, ' ');
        let (kind, id, root, app, title) = (w.next()?, w.next()?, w.next()? == "1", w.next()?, w.next()?);
        Some(serde_json::json!({ "issue": kind == "issue", "id": id, "root": root, "app": app, "title": title, "detail": detail }))
    }).collect();
    serde_json::Value::Array(v)
}

fn fix_tasks() -> serde_json::Value {
    let (_, o) = cmd_out(&["shadow", "fix", "tasks", "--porcelain"]);
    serde_json::Value::Array(o.lines().filter_map(|l| {
        let (id, label) = l.split_once('|')?;
        Some(serde_json::json!({ "id": id, "label": label }))
    }).collect())
}

fn safe_id(s: &str) -> bool {
    !s.is_empty() && s.len() < 80 && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
}

/// a repair: user-level ones run here; ones that need the password open a terminal (sudo asks)
fn fix_apply(id: &str, root: bool) -> Result<String, String> {
    if !safe_id(id) {
        return Err("bad id".into());
    }
    if root {
        spawn(&shadow_apps::terminal_exec(&format!("shadow fix apply {id}; echo; read -p 'Enter closes this window' _")));
        return Ok("opened a terminal — sudo asks for your password there".into());
    }
    let (c, o) = cmd_out(&["shadow", "fix", "apply", id]);
    if c == 0 { Ok(o.trim().to_string()) } else { Err(o.trim().to_string()) }
}

fn fix_run(id: &str) -> Result<String, String> {
    if !safe_id(id) {
        return Err("bad id".into());
    }
    let (c, o) = cmd_out(&["shadow", "fix", "run", id]);
    if c == 0 { Ok(o.trim().to_string()) } else { Err(o.trim().to_string()) }
}

// ── default apps ───────────────────────────────────────────────────────────────────────────────

fn defaults_get() -> serde_json::Value {
    let text = conf();
    let have = |c: &str| std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(c).is_file()));
    let terms: Vec<&str> = shadow_apps::TERMINALS.iter().map(|t| t.0).filter(|t| have(t)).collect();
    let files: Vec<&str> = ["shadow-files", "nautilus", "dolphin", "thunar", "nemo", "pcmanfm"].into_iter().filter(|t| have(t)).collect();
    serde_json::json!({
        "terminal": get_in(&text, "defaults", "terminal_cmd").map(|v| v.trim_matches('"').to_string()).unwrap_or_default(),
        "files": get_in(&text, "defaults", "files_cmd").map(|v| v.trim_matches('"').to_string()).unwrap_or_default(),
        "terminals": terms, "file_managers": files,
    })
}

fn defaults_set(j: &serde_json::Value) -> Result<String, String> {
    let mut t = conf();
    for (k, key) in [("terminal", "terminal_cmd"), ("files", "files_cmd")] {
        if let Some(v) = j.get(k).and_then(|v| v.as_str()) {
            if v.len() > 200 || v.contains('\n') {
                return Err("too long".into());
            }
            t = set_in(&t, "defaults", key, &serde_json::Value::String(v.into()).to_string());
        }
    }
    apply(&t)
}

// ── crash log ──────────────────────────────────────────────────────────────────────────────────

fn crash_get() -> serde_json::Value {
    let dir = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state")).join("shadow-tui");
    let mut out = vec![];
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "log") {
            let t = std::fs::read_to_string(&p).unwrap_or_default();
            for l in t.lines().filter(|l| l.contains("panicked") || l.contains("ERROR") || l.contains("crash")).rev().take(20) {
                out.push(serde_json::json!({ "file": p.file_name().map(|f| f.to_string_lossy().to_string()), "line": l.chars().take(400).collect::<String>() }));
            }
        }
    }
    serde_json::Value::Array(out)
}

// ── raw config ─────────────────────────────────────────────────────────────────────────────────

fn config_put(text: &str) -> Result<String, String> {
    text.parse::<shadow_ui::theme::TomlTable>().map_err(|e| format!("not valid TOML: {}", e.to_string().lines().next().unwrap_or("")))?;
    apply(text)
}

// ── routing ────────────────────────────────────────────────────────────────────────────────────

fn handle(mut s: TcpStream, sh: Arc<Mutex<Shared>>) {
    let Some(req) = read_req(&mut s) else { return };
    let port = port();
    // DNS rebinding: only our own address
    let host_ok = req.header("host").is_some_and(|h| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}"));
    if !host_ok {
        return json_err(&mut s, 403, "wrong host");
    }
    let origin_ok = req.header("origin").map(|o| o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")).unwrap_or(true);
    if !origin_ok {
        return json_err(&mut s, 403, "cross-site request refused");
    }
    let path = req.path.split('?').next().unwrap_or("/").to_string();
    // the app shell itself holds nothing secret
    match (req.method.as_str(), path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => return respond(&mut s, 200, "text/html; charset=utf-8", INDEX.as_bytes(), &[]),
        ("GET", "/app.js") => return respond(&mut s, 200, "text/javascript", APP_JS.as_bytes(), &[]),
        ("GET", "/style.css") => return respond(&mut s, 200, "text/css", STYLE.as_bytes(), &[]),
        ("GET", "/sw.js") => return respond(&mut s, 200, "text/javascript", SW.as_bytes(), &[("Service-Worker-Allowed", "/".into())]),
        ("GET", "/manifest.webmanifest") => return respond(&mut s, 200, "application/manifest+json", MANIFEST.as_bytes(), &[]),
        ("GET", "/icon.svg") => return respond(&mut s, 200, "image/svg+xml", ICON.as_bytes(), &[]),
        ("GET", "/font.ttf") => return respond(&mut s, 200, "font/ttf", FONT, &[]),
        _ => {}
    }
    let (key, cookie) = {
        let g = sh.lock().unwrap();
        (g.key.clone(), g.cookie.clone())
    };
    let has_cookie = req.header("cookie").is_some_and(|c| c.split(';').any(|kv| kv.trim().strip_prefix("sk=").is_some_and(|v| same(v, &cookie))));
    let has_key = req.header("x-shadow-key").is_some_and(|k| same(k, &key));
    // trade the key from the URL fragment for the session cookie
    if req.method == "POST" && path == "/api/unlock" {
        if !has_key {
            return json_err(&mut s, 401, "the key is from another session — open settings from Shadow (Mod+Comma)");
        }
        return respond(&mut s, 200, "application/json", b"{\"ok\":true}", &[("Set-Cookie", format!("sk={cookie}; HttpOnly; SameSite=Strict; Path=/"))]);
    }
    if !(has_cookie || has_key) {
        return json_err(&mut s, 401, "locked — open settings from Shadow (Mod+Comma) to unlock this session");
    }
    if matches!(req.method.as_str(), "POST" | "PUT") && !req.header("content-type").is_some_and(|c| c.starts_with("application/json")) {
        return json_err(&mut s, 400, "JSON only");
    }
    let j = req.json();
    let res: Result<serde_json::Value, String> = match (req.method.as_str(), path.as_str()) {
        ("GET", "/api/info") => {
            let os = std::fs::read_to_string("/etc/os-release").ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))).unwrap_or_default();
            Ok(serde_json::json!({ "version": env!("CARGO_PKG_VERSION"), "os": os, "host": std::fs::read_to_string("/etc/hostname").unwrap_or_default().trim(), "compositor": shadow_niri::shadow_compositor().is_some() }))
        }
        ("GET", "/api/settings") => Ok(settings_get()),
        ("POST", "/api/settings") => settings_set(&j).map(|m| serde_json::json!({ "ok": m })),
        ("GET", "/api/fix/issues") => Ok(fix_issues()),
        ("GET", "/api/fix/tasks") => Ok(fix_tasks()),
        ("POST", "/api/fix/apply") => fix_apply(j["id"].as_str().unwrap_or(""), j["root"].as_bool().unwrap_or(false)).map(|m| serde_json::json!({ "ok": m })),
        ("POST", "/api/fix/run") => fix_run(j["id"].as_str().unwrap_or("")).map(|m| serde_json::json!({ "ok": m })),
        ("POST", "/api/fix/console") => match j["what"].as_str() {
            Some("passfix") => {
                spawn(&shadow_apps::terminal_exec("shadow-passfix"));
                Ok(serde_json::json!({ "ok": "opened the password repair console" }))
            }
            Some("update") => {
                spawn(&shadow_apps::terminal_exec("shadow fix update"));
                Ok(serde_json::json!({ "ok": "opened the update console" }))
            }
            _ => Err("unknown console".into()),
        },
        ("GET", "/api/defaults") => Ok(defaults_get()),
        ("POST", "/api/defaults") => defaults_set(&j).map(|m| serde_json::json!({ "ok": m })),
        ("GET", "/api/config") => Ok(serde_json::json!({ "text": conf() })),
        ("PUT", "/api/config") => config_put(j["text"].as_str().unwrap_or("")).map(|m| serde_json::json!({ "ok": m })),
        ("GET", "/api/crash") => Ok(crash_get()),
        ("POST", "/api/crash/report") => {
            let (c, o) = cmd_out(&["shadow-prefs", "report"]);
            if c == 0 { Ok(serde_json::json!({ "ok": o.trim() })) } else { Err(o.trim().to_string()) }
        }
        _ => return json_err(&mut s, 404, "no such endpoint"),
    };
    match res {
        Ok(v) => json_ok(&mut s, v),
        Err(e) => json_err(&mut s, 400, &e),
    }
}

fn serve() -> i32 {
    let port = port();
    let l = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("shadow-webd: 127.0.0.1:{port}: {e} (already running?)");
            return 1;
        }
    };
    let key = random_hex(32);
    let cookie = random_hex(32);
    let dir = runtime_dir();
    let _ = std::fs::create_dir_all(&dir);
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let kp = dir.join("key");
    let _ = std::fs::remove_file(&kp);
    {
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&kp) {
            Ok(mut f) => {
                let _ = f.write_all(key.as_bytes());
            }
            Err(e) => {
                eprintln!("shadow-webd: cannot write the key: {e}");
                return 1;
            }
        }
    }
    eprintln!("shadow-webd: http://127.0.0.1:{port}/ (key in {})", kp.display());
    let sh = Arc::new(Mutex::new(Shared { key, cookie }));
    for s in l.incoming().flatten() {
        let sh = sh.clone();
        std::thread::spawn(move || handle(s, sh));
    }
    0
}

/// the URL that unlocks the app for this session
pub fn url(page: &str) -> Option<String> {
    let key = std::fs::read_to_string(runtime_dir().join("key")).ok()?;
    Some(format!("http://127.0.0.1:{}/#k={}&p={}", port(), key.trim(), page))
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.first().map(|s| s.as_str()) {
        None => std::process::exit(serve()),
        Some("url") => match url(a.get(1).map(|s| s.as_str()).unwrap_or("settings")) {
            Some(u) => println!("{u}"),
            None => {
                eprintln!("shadow-webd is not running");
                std::process::exit(1)
            }
        },
        _ => {
            eprintln!("usage: shadow-webd [url [PAGE]]");
            std::process::exit(2)
        }
    }
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn values_are_validated_into_toml_literals() {
        assert_eq!(literal(&num(0.0, 2000.0, 40.0), &serde_json::json!(600)), Ok("600".into()));
        assert!(literal(&num(0.0, 2000.0, 40.0), &serde_json::json!(9000)).is_err());
        assert_eq!(literal(&num(-1.0, 1.0, 0.1), &serde_json::json!(0.5)), Ok("0.5".into()));
        assert_eq!(literal(&kind("bool"), &serde_json::json!(true)), Ok("true".into()));
        assert_eq!(literal(&choice(&["on", "low", "off"]), &serde_json::json!("low")), Ok("\"low\"".into()));
        assert!(literal(&choice(&["on", "off"]), &serde_json::json!("rm -rf")).is_err());
        assert_eq!(literal(&kind("color"), &serde_json::json!("#D4AA5A")), Ok("\"#D4AA5A\"".into()));
        assert!(literal(&kind("color"), &serde_json::json!("red\"]\nx=1")).is_err());
        // text is escaped as a TOML basic string, newlines refused
        assert_eq!(literal(&kind("text"), &serde_json::json!("us,ca")), Ok("\"us,ca\"".into()));
        assert!(literal(&kind("text"), &serde_json::json!("a\nb")).is_err());
        assert!(safe_id("firefox-profile-lock") && !safe_id("x; rm -rf ~") && !safe_id(""));
        assert!(same("abc", "abc") && !same("abc", "abd") && !same("abc", "ab"));
    }
}
