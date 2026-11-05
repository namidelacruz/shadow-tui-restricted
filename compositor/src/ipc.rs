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
    st.cur = st.desk.focused.min(st.desk.screens.len().saturating_sub(1));
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    match verb {
        "ping" => "pong".into(),
        // the questions Shadow's apps ask niri (`niri msg …`), answered in niri's JSON shapes, so the
        // hub, launcher and session services work unchanged on this compositor
        "niri" => niri_compat(st, rest),
        "state" => {
            let center = st.ring().center();
            let wins: Vec<serde_json::Value> = st.ring().order.iter().enumerate().filter_map(|(i, id)| st.win(*id).map(|w| serde_json::json!({
                "id": w.id, "slot": i, "app_id": w.app_id, "title": w.title, "pid": w.pid, "preset": w.size.preset, "fullscreen": w.size.full, "pointer": w.pointer,
            }))).collect();
            serde_json::json!({
                "center": center, "windows": wins, "refresh_hz": st.refresh_hz, "locked": st.ui.is_some() && !st.greeter_mode,
                "hud": st.hud_visible, "pointer_live": st.pointer_live, "lua_modules": st.lua.as_ref().map(|l| l.modules()).unwrap_or_default(),
                "config_problem": st.cfg_problem,
                "pointer": [st.pointer_loc.x, st.pointer_loc.y],
                "tutorial": st.tutorial.as_ref().map(|t| serde_json::json!({
                    "step": t.at, "steps": t.steps.len(), "done": t.done,
                    "buttons": st.tut_buttons.iter().map(|(n, r)| serde_json::json!({ "name": n, "x": r.x, "y": r.y, "w": r.w, "h": r.h })).collect::<Vec<_>>(),
                })),
            }).to_string()
        }
        // the web shell (taskbar, start menu): every window, screen and workspace in one answer
        "shell" => shell_state(st),
        "close" => match rest.trim().parse::<u64>() {
            Ok(id) if st.win(id).is_some() => {
                st.focus_window(id);
                st.close_center();
                "ok".into()
            }
            _ => "error: no such window".into(),
        },
        "minimize" => match rest.trim().parse::<u64>() {
            Ok(id) if st.win(id).is_some() => {
                let si = st.desk.locate(id).map(|l| l.0).unwrap_or(st.cur);
                st.with_screen(si, |s| s.minimize(id));
                "ok".into()
            }
            _ => "error: no such window".into(),
        },
        // "workspace SCREEN N": show workspace N (1-based) on that screen
        "workspace" => {
            let v: Vec<usize> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            match v.as_slice() {
                [si, n] if *si < st.desk.screens.len() && (1..=crate::desk::WORKSPACES).contains(n) => {
                    crate::input::switch_workspace(st, *si, n - 1);
                    "ok".into()
                }
                _ => "error: workspace SCREEN N".into(),
            }
        }
        "action" => {
            crate::input::run_action(st, rest);
            "ok".into()
        }
        "spawn" => {
            crate::input::spawn(st, rest);
            "ok".into()
        }
        "focus" => match rest.trim().parse::<u64>() {
            Ok(id) if st.win(id).is_some() => {
                st.focus_window(id);
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
        // test hook for automated tests only (the variable is never set in a real session)
        "test-click" if std::env::var_os("SHADOW_TEST_INPUT").is_some() => {
            let v: Vec<f64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if v.len() == 2 {
                crate::input::test_click(st, v[0], v[1]);
                "ok".into()
            } else {
                "error: test-click X Y".into()
            }
        }
        // a widget about to open an app: the next window unfolds from this rect's centre
        "origin" => {
            let v: Vec<f64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
            if v.len() == 4 {
                st.pending_origin = Some(((v[0] + v[2] / 2.0, v[1] + v[3] / 2.0), std::time::Instant::now()));
                "ok".into()
            } else {
                "error: origin X Y W H".into()
            }
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
                st.set_output_scale(None, crate::winit::scale_override(st.cfg.look.scale));
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

fn glyph_name(g: crate::glyphs::G) -> String {
    format!("{g:?}").to_ascii_lowercase()
}

/// windows, screens and workspaces for the taskbar / start menu (JSON)
fn shell_state(st: &State) -> String {
    let focused_screen = st.desk.focused;
    let center = st.desk.screens.get(focused_screen).map(|_| st.desk.ring(focused_screen).center()).unwrap_or(None);
    let screens: Vec<serde_json::Value> = st.desk.screens.iter().enumerate().map(|(i, s)| serde_json::json!({
        "index": i, "name": s.name, "internal": s.internal, "primary": i == st.desk.primary(), "focused": i == focused_screen,
        "x": s.loc.0, "y": s.loc.1, "w": s.size.0, "h": s.size.1, "workspace": s.active + 1,
        "workspaces": s.ws.iter().enumerate().map(|(n, r)| serde_json::json!({ "n": n + 1, "windows": r.all().count(), "active": n == s.active })).collect::<Vec<_>>(),
    })).collect();
    let mut wins = vec![];
    for (id, si, wi) in st.desk.all() {
        let Some(w) = st.win(id) else { continue };
        let g = crate::glyphs::for_app(&w.app_id);
        let minimized = st.desk.screens[si].ws[wi].min.contains(&id);
        wins.push(serde_json::json!({
            "id": id, "app_id": w.app_id, "title": w.title, "pid": w.pid, "screen": si, "workspace": wi + 1,
            "minimized": minimized, "maximized": w.size.max, "fullscreen": w.size.full,
            "focused": Some(id) == center, "visible": st.desk.screens[si].active == wi,
            "glyph": glyph_name(g), "jp": crate::glyphs::jp(g), "en": crate::glyphs::en(g),
        }));
    }
    // taskbar order: by id (the order apps were opened), as Windows does
    wins.sort_by_key(|w| w["id"].as_u64().unwrap_or(0));
    serde_json::json!({ "screens": screens, "windows": wins, "focused": center, "locked": st.ui.is_some() }).to_string()
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
    let focused = st.ring().center();
    match words.as_slice() {
        ["version"] => {
            let v = env!("CARGO_PKG_VERSION");
            if json {
                serde_json::json!({ "cli": format!("shadow-compositor {v}"), "compositor": format!("shadow-compositor {v}") }).to_string()
            } else {
                format!("Compositor version: shadow-compositor {v}")
            }
        }
        // every screen's nine workspaces (ids: screen × 10 + n)
        ["workspaces"] => serde_json::Value::Array(st.desk.screens.iter().enumerate().flat_map(|(si, s)| {
            let focused = si == st.desk.focused;
            s.ws.iter().enumerate().map(move |(n, r)| serde_json::json!({
                "id": si * 10 + n + 1, "idx": n + 1, "name": null, "output": s.name, "is_active": n == s.active,
                "is_focused": focused && n == s.active, "active_window_id": r.center(),
            })).collect::<Vec<_>>()
        }).collect()).to_string(),
        ["focused-output"] => serde_json::json!({ "name": st.desk.screens.get(st.desk.focused).map(|s| s.name.clone()).unwrap_or(output) }).to_string(),
        ["outputs"] => {
            // niri's shape, so shadow-displayd and the settings app read modes and scale
            let mut m = serde_json::Map::new();
            for s in &st.desk.screens {
                let o = &s.out;
                let modes: Vec<_> = o.modes();
                let cur = o.current_mode().and_then(|c| modes.iter().position(|m| *m == c));
                let pref = o.preferred_mode();
                let pp = o.physical_properties();
                let modes_j: Vec<_> = modes.iter().map(|m| serde_json::json!({ "width": m.size.w, "height": m.size.h, "refresh_rate": m.refresh, "is_preferred": Some(*m) == pref })).collect();
                m.insert(s.name.clone(), serde_json::json!({
                    "name": s.name, "make": pp.make, "model": pp.model, "physical_size": [pp.size.w, pp.size.h], "modes": modes_j, "current_mode": cur,
                    "logical": { "x": s.loc.0 as i32, "y": s.loc.1 as i32, "width": s.size.0 as i32, "height": s.size.1 as i32, "scale": o.current_scale().fractional_scale(), "transform": "Normal" },
                }));
            }
            serde_json::Value::Object(m).to_string()
        }
        // shadow-displayd: scale changes apply live; mode / power are handled by the compositor itself
        ["output", name, "scale", v] => match v.parse::<f64>() {
            Ok(x) if (0.5..=4.0).contains(&x) => {
                st.set_output_scale(Some(name), x);
                "ok".into()
            }
            _ => "error: scale must be 0.5-4".into(),
        },
        ["output", _, "mode", ..] | ["output", _, "on"] | ["output", _, "off"] => "ok".into(),
        ["focused-window"] => match focused.and_then(|id| st.win(id)) {
            Some(w) => serde_json::json!({ "id": w.id, "app_id": w.app_id, "title": w.title, "pid": w.pid }).to_string(),
            None => "null".into(),
        },
        ["windows"] => serde_json::Value::Array(st.desk.all().into_iter().filter_map(|(id, si, wi)| st.win(id).map(|w| serde_json::json!({ "id": w.id, "app_id": w.app_id, "title": w.title, "pid": w.pid, "workspace_id": si * 10 + wi + 1, "is_focused": Some(w.id) == focused }))).collect()).to_string(),
        ["action", "quit", ..] => {
            crate::input::run_action(st, "quit");
            "ok".into()
        }
        ["action", "spawn", "--", cmd @ ..] | ["action", "spawn", cmd @ ..] => {
            crate::input::spawn(st, &cmd.join(" "));
            "ok".into()
        }
        // niri's actions that Shadow's apps send, on this compositor's ring
        ["action", a, rest @ ..] => {
            let mapped = match *a {
                "toggle-overview" | "center-column" | "move-window-to-floating" | "toggle-window-floating" | "power-off-monitors" => Some(""),
                "focus-column-right-or-first" | "focus-column-right" | "focus-window-down" => Some("focus-next"),
                "focus-column-left-or-last" | "focus-column-left" | "focus-window-up" => Some("focus-prev"),
                "switch-preset-column-width" => Some("width-up"),
                "switch-preset-column-width-back" => Some("width-down"),
                "fullscreen-window" | "maximize-column" => Some("fullscreen"),
                "close-window" => Some("close"),
                "show-hotkey-overlay" => Some("tutorial open"),
                "set-column-width" => {
                    // "NN%": the nearest width preset
                    let want = rest.first().and_then(|v| v.trim_end_matches('%').parse::<f64>().ok()).unwrap_or(65.0);
                    if let Some(id) = focused {
                        let presets = st.cfg.layout.presets.clone();
                        let best = presets.iter().enumerate().min_by(|a, b| (*a.1 as f64 - want).abs().partial_cmp(&(*b.1 as f64 - want).abs()).unwrap()).map(|x| x.0).unwrap_or(0);
                        st.transition(|_| {});
                        if let Some(w) = st.wins.iter_mut().find(|w| w.id == id) {
                            w.size.preset = best;
                            w.size.full = false;
                        }
                        st.relayout();
                    }
                    Some("")
                }
                "focus-window" => {
                    let id = rest.iter().skip_while(|x| **x != "--id").nth(1).and_then(|v| v.parse::<u64>().ok());
                    if let Some(id) = id.filter(|id| st.win(*id).is_some()) {
                        st.focus_window(id);
                    }
                    Some("")
                }
                _ => None,
            };
            match mapped {
                Some("") => "ok".into(),
                Some(act) => {
                    crate::input::run_action(st, act);
                    "ok".into()
                }
                None => format!("error: shadow-compositor has no niri action `{a}`"),
            }
        }
        _ => format!("error: shadow-compositor does not answer `{args}`"),
    }
}
