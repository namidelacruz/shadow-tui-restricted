//! Control socket ($XDG_RUNTIME_DIR/shadow-compositor.<display>.sock, exported as SHADOW_SOCKET):
//! one command line in, one reply out. Used by shadowctl, the settings app and tests.
use crate::state::State;
use smithay::reexports::calloop::{generic::Generic, Interest, Mode, PostAction};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

pub fn socket_path(display: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join(format!("shadow-compositor.{display}.sock"))
}

pub fn init(state: &mut State) -> std::io::Result<()> {
    let p = socket_path(&state.socket_name);
    let _ = std::fs::remove_file(&p);
    let l = UnixListener::bind(&p)?;
    l.set_nonblocking(true)?;
    std::env::set_var("SHADOW_SOCKET", &p);
    state.handle.insert_source(Generic::new(l, Interest::READ, Mode::Level), |_, l, st| {
        while let Ok((s, _)) = l.accept() {
            handle_client(st, s);
        }
        Ok(PostAction::Continue)
    }).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

fn handle_client(st: &mut State, s: UnixStream) {
    let _ = s.set_nonblocking(false);
    let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
    let mut line = String::new();
    if BufReader::new(&s).read_line(&mut line).is_err() {
        return;
    }
    let reply = command(st, line.trim());
    let mut s = s;
    let _ = s.write_all(reply.as_bytes());
    let _ = s.write_all(b"\n");
}

pub fn command(st: &mut State, line: &str) -> String {
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    match verb {
        "ping" => "pong".into(),
        // the questions Shadow's apps ask niri (`niri msg …`), answered in niri's JSON shapes, so the
        // hub, launcher and session services work unchanged on this compositor
        "niri" => niri_compat(st, rest),
        "state" => {
            let center = st.ring.center();
            let wins: Vec<serde_json::Value> = st.ring.order.iter().enumerate().filter_map(|(i, id)| st.win(*id).map(|w| serde_json::json!({
                "id": w.id, "slot": i, "app_id": w.app_id, "title": w.title, "pid": w.pid, "preset": w.size.preset, "fullscreen": w.size.full, "pointer": w.pointer,
            }))).collect();
            serde_json::json!({
                "center": center, "windows": wins, "refresh_hz": st.refresh_hz, "locked": st.ui.is_some() && !st.greeter_mode,
                "hud": st.hud_visible, "pointer_live": st.pointer_live, "lua_modules": st.lua.as_ref().map(|l| l.modules()).unwrap_or_default(),
                "config_problem": st.cfg_problem,
            }).to_string()
        }
        "action" => {
            crate::input::run_action(st, rest);
            "ok".into()
        }
        "spawn" => {
            crate::input::spawn(st, rest);
            "ok".into()
        }
        "focus" => match rest.parse::<u64>() {
            Ok(id) if st.win(id).is_some() => {
                st.transition(|r| { r.focus(id); });
                "ok".into()
            }
            _ => "error: no such window".into(),
        },
        "lock" => {
            crate::input::lock(st);
            "ok".into()
        }
        // test hook, demo mode only (never available on a real login or lock screen)
        "ui-type" | "ui-key" if std::env::var_os("SHADOW_GREET_DEMO").is_some() => {
            use smithay::input::keyboard::{xkb, Keysym};
            if verb == "ui-type" {
                for c in rest.chars() {
                    crate::input::ui_key(st, Keysym::new(c as u32), Some(c));
                }
            } else {
                let k = xkb::keysym_from_name(rest, xkb::KEYSYM_CASE_INSENSITIVE);
                crate::input::ui_key(st, k, None);
            }
            "ok".into()
        }
        "refresh" => match rest.parse::<u32>() {
            Ok(hz) => {
                st.wanted_refresh = Some(hz);
                crate::backend_apply_refresh(st);
                format!("ok {}", st.refresh_hz)
            }
            Err(_) => "error: refresh HZ".into(),
        },
        "config-reload" => {
            let (c, p) = crate::config::load(None);
            st.cfg = c;
            st.cfg_problem = p.clone();
            st.binds = crate::input::compile_binds(st);
            if matches!(st.backend, crate::state::Backend::Udev(_)) {
                st.set_output_scale(crate::winit::scale_override(st.cfg.look.scale));
            }
            st.relayout();
            p.map(|p| format!("error: {p} (defaults in use)")).unwrap_or_else(|| "ok".into())
        }
        "lua-reload" => {
            crate::luahost::reload(st);
            format!("ok {}", st.lua.as_ref().map(|l| l.modules().join(",")).unwrap_or_default())
        }
        _ => format!("error: unknown command '{verb}' (ping, state, action A, spawn CMD, focus ID, lock, refresh HZ, config-reload, lua-reload)"),
    }
}

/// talk to shadow-healer's socket (revert / status); a short reply line
pub fn healer(cmd: &str) -> String {
    let p = std::env::var("SHADOW_HEALER_SOCK").unwrap_or_else(|_| "/run/shadow-healer.sock".into());
    match UnixStream::connect(p) {
        Ok(mut s) => {
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = s.write_all(format!("{cmd}\n").as_bytes());
            let mut r = String::new();
            let _ = BufReader::new(&s).read_line(&mut r);
            if r.trim().is_empty() { "no reply".into() } else { r.trim().to_string() }
        }
        Err(_) => "healer not running".into(),
    }
}

/// `niri msg [--json] <what>` equivalents. Unknown questions answer "error: …" (the caller treats
/// that like niri refusing).
fn niri_compat(st: &mut State, args: &str) -> String {
    let json = args.split_whitespace().any(|a| a == "--json" || a == "-j");
    let words: Vec<&str> = args.split_whitespace().filter(|a| *a != "--json" && *a != "-j").collect();
    let output = st.output_name();
    let focused = st.ring.center();
    match words.as_slice() {
        ["version"] => {
            let v = env!("CARGO_PKG_VERSION");
            if json {
                serde_json::json!({ "cli": format!("shadow-compositor {v}"), "compositor": format!("shadow-compositor {v}") }).to_string()
            } else {
                format!("Compositor version: shadow-compositor {v}")
            }
        }
        // one workspace: the ring
        ["workspaces"] => serde_json::json!([{ "id": 1, "idx": 1, "name": null, "output": output, "is_active": true, "is_focused": true, "active_window_id": focused }]).to_string(),
        ["focused-output"] => serde_json::json!({ "name": output }).to_string(),
        ["outputs"] => serde_json::json!({ output.clone(): { "name": output, "make": "", "model": "", "logical": { "scale": st.scale() } } }).to_string(),
        ["focused-window"] => match focused.and_then(|id| st.win(id)) {
            Some(w) => serde_json::json!({ "id": w.id, "app_id": w.app_id, "title": w.title, "pid": w.pid }).to_string(),
            None => "null".into(),
        },
        ["windows"] => serde_json::Value::Array(st.ring.order.iter().filter_map(|id| st.win(*id)).map(|w| serde_json::json!({ "id": w.id, "app_id": w.app_id, "title": w.title, "pid": w.pid, "workspace_id": 1, "is_focused": Some(w.id) == focused })).collect()).to_string(),
        ["action", "quit", ..] => {
            crate::input::run_action(st, "quit");
            "ok".into()
        }
        ["action", "spawn", "--", cmd @ ..] | ["action", "spawn", cmd @ ..] => {
            crate::input::spawn(st, &cmd.join(" "));
            "ok".into()
        }
        ["action", "toggle-overview"] => "ok".into(),
        _ => format!("error: shadow-compositor does not answer `{args}`"),
    }
}
