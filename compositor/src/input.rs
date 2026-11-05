//! Input. Keys go through the bind table, then to the focused client (centre stage, or a layer
//! surface that holds the keyboard). The mouse works everywhere (input.pointer = "all"): the hub
//! and other Shadow layers, the centre window and its menus, and the ring — click a wireframe node
//! to bring it to the centre, Mod+scroll turns the ring. input.pointer = "apps" restores the
//! keyboard-first mask for centre windows not listed in pointer_apps.
use crate::state::State;
use crate::ui::{Mode, Ui, UiAction, UiMsg};
use smithay::{
    backend::input::{AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent},
    input::{
        keyboard::{keysyms, xkb, FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    desktop::{layer_map_for_output, LayerSurface, WindowSurfaceType},
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::shell::wlr_layer::Layer,
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Mods {
    pub logo: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

pub struct Bind {
    pub mods: Mods,
    pub sym: Keysym,
    pub action: String,
}

/// "Mod+Shift+Tab" → (mods, keysym). `Mod` is Super, or Alt when input.modifier = "alt".
pub fn parse_bind(key: &str, modkey: &str) -> Option<(Mods, Keysym)> {
    let mut m = Mods::default();
    let parts: Vec<&str> = key.split('+').collect();
    let (last, mods) = parts.split_last()?;
    for p in mods {
        match p.to_lowercase().as_str() {
            "mod" => if modkey == "alt" { m.alt = true } else { m.logo = true },
            "super" | "logo" | "win" => m.logo = true,
            "alt" => m.alt = true,
            "ctrl" | "control" => m.ctrl = true,
            "shift" => m.shift = true,
            _ => return None,
        }
    }
    let sym = xkb::keysym_from_name(last, xkb::KEYSYM_CASE_INSENSITIVE);
    if sym.raw() == keysyms::KEY_NoSymbol {
        return None;
    }
    Some((m, sym))
}

pub fn compile_binds(state: &State) -> Vec<Bind> {
    let mut v = vec![];
    for (k, a) in &state.cfg.binds {
        match parse_bind(k, &state.cfg.input.modifier) {
            Some((mods, sym)) => v.push(Bind { mods, sym, action: a.clone() }),
            None => tracing::warn!("ignoring bind with unknown key: {k}"),
        }
    }
    if let Some(l) = state.lua.as_ref() {
        for (k, idx) in l.binds() {
            if let Some((mods, sym)) = parse_bind(&k, &state.cfg.input.modifier) {
                v.push(Bind { mods, sym, action: format!("lua {idx}") });
            }
        }
    }
    v
}

/// case-fold Latin letters (keysyms A..Z are 0x41..0x5a, a..z 0x61..0x7a)
fn lower(s: Keysym) -> u32 {
    let r = s.raw();
    if (0x41..=0x5a).contains(&r) { r + 0x20 } else { r }
}

enum Act {
    Run(String),
    Ui(Keysym, Option<char>),
    Vt(i32),
    Nothing,
}

pub fn on_key(state: &mut State, code: smithay::input::keyboard::Keycode, ks: KeyState, time: u32) {
    state.last_input = Instant::now();
    activity(state);
    if ks == KeyState::Pressed && state.cfg.input.hide_cursor_when_typing && !state.cursor_typing_hidden {
        state.cursor_typing_hidden = true;
        state.dirty = true;
    }
    if ks == KeyState::Pressed {
        state.key_times.push(Instant::now());
        state.key_times.retain(|t| t.elapsed() < Duration::from_secs(5));
    }
    let Some(kb) = state.seat.get_keyboard() else { return };
    let serial = SERIAL_COUNTER.next_serial();
    let act = kb.input::<Act, _>(state, code, ks, serial, time, |st, mods: &ModifiersState, h| {
        let raw = h.raw_code().raw();
        if ks == KeyState::Released {
            if let Some(i) = st.suppressed_keys.iter().position(|k| *k == raw) {
                st.suppressed_keys.remove(i);
                return FilterResult::Intercept(Act::Nothing);
            }
            return FilterResult::Forward;
        }
        let sym = h.modified_sym();
        let r = sym.raw();
        if (keysyms::KEY_XF86Switch_VT_1..=keysyms::KEY_XF86Switch_VT_12).contains(&r) {
            st.suppressed_keys.push(raw);
            return FilterResult::Intercept(Act::Vt((r - keysyms::KEY_XF86Switch_VT_1 + 1) as i32));
        }
        if st.ui.is_some() {
            st.suppressed_keys.push(raw);
            return FilterResult::Intercept(Act::Ui(sym, sym.key_char()));
        }
        let now = Mods { logo: mods.logo, alt: mods.alt, ctrl: mods.ctrl, shift: mods.shift };
        let raws: Vec<u32> = h.raw_syms().iter().map(|s| lower(*s)).collect();
        if let Some(b) = st.binds.iter().find(|b| b.mods == now && (raws.contains(&lower(b.sym)) || lower(sym) == lower(b.sym))) {
            st.suppressed_keys.push(raw);
            return FilterResult::Intercept(Act::Run(b.action.clone()));
        }
        FilterResult::Forward
    });
    match act {
        Some(Act::Run(a)) => run_action(state, &a),
        Some(Act::Ui(sym, ch)) => ui_key(state, sym, ch),
        Some(Act::Vt(n)) => {
            if let crate::state::Backend::Udev(u) = &mut state.backend {
                u.change_vt(n);
            }
        }
        _ => {}
    }
}

pub fn ui_key(state: &mut State, sym: Keysym, ch: Option<char>) {
    let Some(ui) = state.ui.as_mut() else { return };
    let action = ui.key(sym, ch);
    ui_do(state, action);
}

/// a click on the login / lock card
fn ui_click(state: &mut State) {
    let p = state.pointer_loc - state.desk.screens.get(state.desk.focused).map(|s| Point::from(s.loc)).unwrap_or_default();
    let Some(ui) = state.ui.as_mut() else { return };
    let hit = ui.buttons.iter().find(|(_, r)| r.contains(p.x, p.y)).map(|(n, _)| *n);
    let action = match hit {
        Some(n) => ui.click(n),
        None => UiAction::None,
    };
    ui_do(state, action);
}

fn ui_do(state: &mut State, action: UiAction) {
    state.dirty = true;
    let Some(ui) = state.ui.as_mut() else { return };
    match action {
        UiAction::Submit => {
            ui.busy = true;
            let (mode, user, pass) = (ui.mode, ui.user.clone(), std::mem::take(&mut ui.pass));
            let cmd = ui.sessions.get(ui.sess).map(|s| s.1.clone()).unwrap_or_else(|| "shadow-session".into());
            let svc = state.cfg.lock.pam_service.clone();
            let Some(tx) = state.ui_tx.clone() else { return };
            std::thread::spawn(move || {
                let r = match mode {
                    Mode::Greeter => crate::ui::greetd_login(&user, &pass, &cmd),
                    Mode::Lock => crate::pam::authenticate(&svc, &user, &pass),
                };
                let _ = tx.send(match r { Ok(()) => UiMsg::Success, Err(e) => UiMsg::Fail(e) });
            });
        }
        UiAction::Rescue => {
            state.exit_code = 3; // greeter-wrapper switches to the text greeter, which offers a rescue shell
            state.signal.stop();
        }
        UiAction::Suspend => spawn_raw("systemctl suspend"),
        UiAction::Reboot => spawn_raw("systemctl reboot"),
        UiAction::Poweroff => spawn_raw("systemctl poweroff"),
        UiAction::None => {}
    }
}

pub fn on_ui_msg(state: &mut State, msg: UiMsg) {
    let Some(ui) = state.ui.as_mut() else { return };
    ui.busy = false;
    state.dirty = true;
    match msg {
        UiMsg::Success => match ui.mode {
            Mode::Greeter => ui.handoff = Some(Instant::now()),
            Mode::Lock => unlock(state),
        },
        UiMsg::Fail(e) => {
            ui.status = (e.chars().take(90).collect(), 2);
            ui.pass.clear();
        }
    }
}

fn locked_marker() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join("shadow/locked")
}

pub fn lock(state: &mut State) {
    if state.ui.is_some() || state.greeter_mode {
        return;
    }
    state.ui = Some(Ui::new(Mode::Lock));
    let m = locked_marker();
    let _ = std::fs::create_dir_all(m.parent().unwrap());
    let _ = std::fs::write(&m, format!("{}\n", std::process::id()));
    state.relayout();
}

fn unlock(state: &mut State) {
    state.ui = None;
    let _ = std::fs::remove_file(locked_marker());
    state.relayout();
}

fn spawn_raw(cmd: &str) {
    // double fork through sh: the child is reparented, so no zombies and no SIGCHLD handling
    let _ = std::process::Command::new("sh").arg("-c").arg(format!("({cmd}) >/dev/null 2>&1 &")).stdin(std::process::Stdio::null()).status();
}

/// Launch a program. Shadow's own TUI apps stay in shadow.slice (the 20 % memory ceiling);
/// everything else goes to app.slice so user workloads keep the other 80 %.
pub fn spawn(state: &State, cmd: &str) {
    let prog = cmd.split_whitespace().next().unwrap_or("");
    let base = prog.rsplit('/').next().unwrap_or(prog);
    let mem = &state.cfg.memory;
    let in_slice = mem.shadow_apps.iter().any(|a| a == base || cmd.contains(&format!(" {a} ")) || cmd.ends_with(&format!(" {a}")))
        || (base == "shadow-term" && (mem.shell_in_slice || cmd.contains("-e sakura")));
    let have_run = std::path::Path::new("/usr/bin/systemd-run").exists() && std::env::var_os("SHADOW_NO_SCOPES").is_none();
    if in_slice || !have_run {
        spawn_raw(cmd);
    } else {
        let q = cmd.replace('\'', "'\\''");
        spawn_raw(&format!("systemd-run --user --scope --slice=app.slice --collect --quiet -- sh -c '{q}'"));
    }
}

/// any input: idle timers (ext-idle-notify clients such as shadow-sessiond) start over
fn activity(state: &mut State) {
    let seat = state.seat.clone();
    state.idle_notifier.notify_activity(&seat);
}

pub fn run_action(state: &mut State, a: &str) {
    let (verb, rest) = a.split_once(' ').unwrap_or((a, ""));
    // FIELD TRAINING watches every action; the lock step is acknowledged, not run
    let mut trained = false;
    if let Some(t) = state.tutorial.as_mut() {
        let swallow = t.swallows(a);
        trained = t.observe(a);
        state.dirty = true;
        if swallow {
            return;
        }
    }
    match verb {
        "spawn" => spawn(state, rest),
        "focus-next" => state.transition(|r| r.next()),
        "focus-prev" => state.transition(|r| r.prev()),
        // the main tile's share of the app area (2–3 apps)
        "width-up" | "width-down" => {
            let n = state.cfg.layout.presets.len().max(1);
            state.transition(|_| {});
            state.main_preset = if verb == "width-up" { (state.main_preset + 1).min(n - 1) } else { state.main_preset.saturating_sub(1) };
            state.relayout();
        }
        "fullscreen" => {
            let Some(id) = state.ring().center() else { return };
            state.transition(|_| {});
            if let Some(w) = state.wins.iter_mut().find(|w| w.id == id) {
                w.size.toggle_full();
            }
            state.relayout();
        }
        // Mod+M: the focused app goes to the grid · Mod+Shift+M: the last one comes back
        "minimize" => {
            if let Some(id) = rest.parse::<u64>().ok().or(state.ring().center()) {
                state.minimize(id);
            }
        }
        "restore" => {
            let id = rest.parse::<u64>().ok().or(state.ring().min.first().copied());
            if let Some(id) = id {
                state.restore(id);
            }
        }
        // Mod+Up: maximise (again: restore) · Mod+Down: restore down, then minimise
        "maximize" => {
            if let Some(id) = state.ring().center() {
                toggle_max(state, id);
            }
        }
        "unmaximize" => {
            let Some(id) = state.ring().center() else { return };
            if state.win(id).is_some_and(|w| w.size.max || w.size.full) {
                state.transition(|_| {});
                if let Some(w) = state.wins.iter_mut().find(|w| w.id == id) {
                    w.size.max = false;
                    w.size.full = false;
                }
                state.relayout();
            } else {
                state.minimize(id);
            }
        }
        // workspaces of the focused screen (1–9), and screens
        "workspace" | "workspace-next" | "workspace-prev" => {
            let si = state.desk.focused;
            let Some(scr) = state.desk.screens.get(si) else { return };
            let n = scr.ws.len();
            let cur = scr.active;
            let to = match verb {
                "workspace-next" => (cur + 1).min(n - 1),
                "workspace-prev" => cur.saturating_sub(1),
                _ => rest.trim().parse::<usize>().unwrap_or(1).clamp(1, n) - 1,
            };
            switch_workspace(state, si, to);
        }
        "move-to-workspace" | "move-to-workspace-next" | "move-to-workspace-prev" => {
            let si = state.desk.focused;
            let Some(id) = state.ring().center() else { return };
            let Some(scr) = state.desk.screens.get(si) else { return };
            let (n, cur) = (scr.ws.len(), scr.active);
            let to = match verb {
                "move-to-workspace-next" => (cur + 1).min(n - 1),
                "move-to-workspace-prev" => cur.saturating_sub(1),
                _ => rest.trim().parse::<usize>().unwrap_or(1).clamp(1, n) - 1,
            };
            if to != cur {
                let max = state.cfg.layout.max_open;
                state.transition(|r| r.remove(id));
                state.desk.move_to(id, si, to, max);
                // follow it there
                switch_workspace(state, si, to);
            }
        }
        "focus-screen-next" | "focus-screen-prev" => {
            let n = state.desk.screens.len();
            if n > 1 {
                let d = if verb.ends_with("next") { 1 } else { n - 1 };
                state.desk.focused = (state.desk.focused + d) % n;
                state.cur = state.desk.focused;
                state.relayout();
                state.on_focus_changed();
            }
        }
        "move-to-screen-next" | "move-to-screen-prev" => {
            let n = state.desk.screens.len();
            let Some(id) = state.ring().center() else { return };
            if n > 1 {
                let d = if verb.ends_with("next") { 1 } else { n - 1 };
                let to = (state.desk.focused + d) % n;
                let ws = state.desk.screens[to].active;
                let max = state.cfg.layout.max_open;
                state.transition(|r| r.remove(id));
                state.desk.move_to(id, to, ws, max);
                state.desk.focused = to;
                state.cur = to;
                state.transition(|r| { r.focus(id); });
                state.on_focus_changed();
            }
        }
        // the shell around the apps: start menu, widgets, quick settings, taskbar on/off
        "shell" => {
            let role = rest.trim();
            if state.cfg.shell.mode == "glyph" {
                match role {
                    "toggle" => spawn_raw("shadow-glyph toggle"),
                    "widgets" | "quick" => spawn_raw("shadow-glyph summon apps"),
                    _ => spawn_raw("shadow-glyph summon"),
                }
            } else if ["start", "widgets", "quick", "toggle"].contains(&role) {
                tell_panel(&format!("toggle {role}"));
            }
        }
        // Mod+Shift+Return: the focused app takes the main tile
        "promote" => state.transition(|r| r.promote()),
        "grid-scroll" => {
            let d: f64 = rest.parse().unwrap_or(1.0);
            let (_, max, _) = state.grid();
            state.set_grid_scroll((state.grid_scroll() + d * (state.cfg.layout.tile_px as f64 + state.cfg.layout.gap as f64)).clamp(0.0, max));
            state.relayout();
        }
        "close" => state.close_center(),
        "hud-toggle" => {
            state.hud_visible = !state.hud_visible;
            state.relayout();
        }
        "healer-revert" => {
            let r = crate::ipc::healer("revert");
            state.hud_extra = vec![format!("HEALER {}", r.lines().next().unwrap_or("?").to_uppercase())];
            state.dirty = true;
        }
        "lock" => lock(state),
        "notify" => {
            state.hud_extra = vec![rest.chars().take(60).collect()];
            state.dirty = true;
        }
        // the session ends after the logout animation (render.rs); "quit now" skips it
        "quit" => {
            if rest == "now" || state.cfg.layout.anim_ms == 0 {
                state.signal.stop();
            } else if state.logout.is_none() {
                state.logout = Some(Instant::now());
                state.dirty = true;
            }
        }
        "tutorial" => {
            if trained {
                return; // that key press was the "open this guide again" step itself
            }
            if rest == "close" || state.tutorial.as_ref().map(|t| !t.done).unwrap_or(false) && rest != "open" {
                close_tutorial(state);
            } else {
                state.tutorial = Some(crate::tutorial::Tutorial::new(&state.cfg.binds));
                state.dirty = true;
            }
        }
        "lua" => {
            if let (Some(l), Ok(i)) = (state.lua.as_mut(), rest.parse::<usize>()) {
                l.call_bind(i);
            }
            state.run_lua_actions();
        }
        _ => tracing::warn!("unknown action: {a}"),
    }
}

pub fn close_tutorial(state: &mut State) {
    if state.tutorial.take().is_some() {
        crate::tutorial::mark_done();
    }
    state.tut_buttons.clear();
    state.dirty = true;
}

// ---------------------------------------------------------------- pointer
/// What is under the pointer, front to back in the same order the scene is drawn.
pub enum Under {
    /// a client surface and the surface's origin in global logical coordinates
    Surface(WlSurface, Point<f64, Logical>, Option<LayerSurface>),
    /// a minimised app's square in the grid
    Node(u64),
    /// a window's title bar (or one of its buttons)
    Title(u64, crate::orbit::TitleBtn),
    /// a button of the FIELD TRAINING panel
    Tutorial(&'static str),
    Nothing,
}

fn layer_hit(state: &State, p: Point<f64, Logical>, layers: &[Layer]) -> Option<(WlSurface, Point<f64, Logical>, LayerSurface)> {
    let o = state.output()?;
    let map = layer_map_for_output(o);
    for want in layers {
        // later layers are on top: search from the end
        let ls: Vec<LayerSurface> = map.layers_on(*want).rev().cloned().collect();
        for l in ls {
            let Some(g) = map.layer_geometry(&l) else { continue };
            if let Some((s, sp)) = l.surface_under(p - g.loc.to_f64(), WindowSurfaceType::ALL) {
                return Some((s, (sp + g.loc).to_f64(), l));
            }
        }
    }
    None
}

/// What is under global point `p` (on whichever screen it is).
pub fn under(state: &mut State, p: Point<f64, Logical>) -> Under {
    if state.ui.is_some() {
        return Under::Nothing;
    }
    for (name, r) in &state.tut_buttons {
        if p.x >= r.x && p.x < r.x + r.w && p.y >= r.y && p.y < r.y + r.h {
            return Under::Tutorial(name);
        }
    }
    let si = state.desk.screen_at(p.x, p.y).unwrap_or(state.desk.focused);
    state.with_screen(si, |st| {
        let o = st.origin();
        match under_local(st, p - o) {
            Under::Surface(s, at, l) => Under::Surface(s, at + o, l),
            u => u,
        }
    })
}

/// What is under `p` on the current screen, in its coordinates.
fn under_local(state: &State, p: Point<f64, Logical>) -> Under {
    if let Some((s, at, l)) = layer_hit(state, p, &[Layer::Overlay, Layer::Top]) {
        return Under::Surface(s, at, Some(l));
    }
    // open apps, front to back (their popups included), each with its title bar
    let (vis, _) = state.visual();
    let titles = state.cfg.shell.titlebars;
    for id in state.stacking() {
        let Some((_, rect, _, _)) = vis.iter().find(|v| v.0 == id && v.3) else { continue };
        let Some(w) = state.win(id) else { continue };
        if titles && !w.size.full {
            if let Some(b) = crate::orbit::title_hit(*rect, p.x, p.y) {
                return Under::Title(id, b);
            }
        }
        let loc = Point::<f64, Logical>::from((rect.x, rect.y)) - w.window.geometry().loc.to_f64();
        let focused = state.ring().center() == Some(id);
        if !focused || state.center_takes_pointer() {
            if let Some((s, sp)) = w.window.surface_under(p - loc, WindowSurfaceType::ALL) {
                return Under::Surface(s, sp.to_f64() + loc, None);
            }
        } else if rect.contains(p.x, p.y) {
            return Under::Nothing; // keyboard-only window: the click goes nowhere
        }
        // a maximised or fullscreen window covers the tiles under it
        if (w.size.max || w.size.full) && rect.contains(p.x, p.y) {
            return Under::Nothing;
        }
    }
    // minimised squares
    for (id, r, _, _) in vis.iter().filter(|v| !v.3) {
        if r.contains(p.x, p.y) {
            return Under::Node(*id);
        }
    }
    if let Some((s, at, l)) = layer_hit(state, p, &[Layer::Bottom, Layer::Background]) {
        return Under::Surface(s, at, Some(l));
    }
    Under::Nothing
}

fn surface_focus(u: &Under) -> Option<(WlSurface, Point<f64, Logical>)> {
    match u {
        Under::Surface(s, at, _) => Some((s.clone(), *at)),
        _ => None,
    }
}

pub fn on_input<B: InputBackend>(state: &mut State, event: InputEvent<B>) {
    // input acts on the screen with the keyboard
    state.cur = state.desk.focused.min(state.desk.screens.len().saturating_sub(1));
    match event {
        InputEvent::Keyboard { event } => on_key(state, event.key_code(), event.state(), event.time_msec()),
        InputEvent::PointerMotion { event } => {
            let d = event.delta();
            let before = state.pointer_loc;
            // an active pointer lock keeps the cursor still; confinement keeps it on its surface
            let mut p = before + d;
            if let Some(s) = state.constrained.clone() {
                let ptr = state.seat.get_pointer().unwrap();
                let kind = smithay::wayland::pointer_constraints::with_pointer_constraint(&s, &ptr, |c| c.filter(|c| c.is_active()).map(|c| matches!(&*c, smithay::wayland::pointer_constraints::PointerConstraint::Locked(_))));
                match kind {
                    Some(true) => p = before,
                    Some(false) => if !matches!(under(state, p), Under::Surface(ref u, _, _) if *u == s) { p = before },
                    None => state.constrained = None,
                }
            }
            pointer_to(state, p, event.time_msec());
            // raw deltas for 3D viewports and games (relative-pointer-v1)
            let focus = surface_focus(&under(state, state.pointer_loc));
            let ptr = state.seat.get_pointer().unwrap();
            ptr.relative_motion(state, focus, &smithay::input::pointer::RelativeMotionEvent { delta: d, delta_unaccel: event.delta_unaccel(), utime: event.time() });
            ptr.frame(state);
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let (w, h) = state.total_size();
            let p = event.position_transformed((w as i32, h as i32).into());
            pointer_to(state, p, event.time_msec());
        }
        InputEvent::PointerButton { event } => pointer_button(state, event.button_code(), event.state(), event.time_msec()),
        InputEvent::PointerAxis { event } => {
            activity(state);
            if !state.pointer_live {
                return;
            }
            // over the minimised squares: the grid scrolls (wheel or two fingers, either axis)
            let si = state.desk.screen_at(state.pointer_loc.x, state.pointer_loc.y).unwrap_or(state.desk.focused);
            let px = |ax| event.amount(ax).or(event.amount_v120(ax).map(|d| d / 120.0 * 48.0)).unwrap_or(0.0);
            let d = px(Axis::Vertical) + px(Axis::Horizontal);
            let in_band = state.with_screen(si, |st| {
                let hit = st.grid().2.map(|b| b.contains(st.pointer_loc.x, st.pointer_loc.y)).unwrap_or(false);
                if hit && d != 0.0 {
                    let (_, max, _) = st.grid();
                    st.set_grid_scroll((st.grid_scroll() + d).clamp(0.0, max));
                    st.dirty = true;
                }
                hit
            });
            if in_band {
                return;
            }
            // Mod + scroll: the keyboard moves between the open apps
            let mods = state.seat.get_keyboard().map(|k| k.modifier_state()).unwrap_or_default();
            let modkey = if state.cfg.input.modifier == "alt" { mods.alt } else { mods.logo };
            if modkey {
                let v = event.amount_v120(Axis::Vertical).map(|d| d / 120.0).or(event.amount(Axis::Vertical).map(|d| d / 15.0)).unwrap_or(0.0);
                state.ring_scroll += v;
                while state.ring_scroll.abs() >= 1.0 {
                    if state.ring_scroll > 0.0 {
                        state.transition(|r| r.next());
                        state.ring_scroll -= 1.0;
                    } else {
                        state.transition(|r| r.prev());
                        state.ring_scroll += 1.0;
                    }
                }
                return;
            }
            let src = event.source();
            let mut f = AxisFrame::new(event.time_msec()).source(src);
            for ax in [Axis::Horizontal, Axis::Vertical] {
                let amt = event.amount(ax).unwrap_or_else(|| event.amount_v120(ax).unwrap_or(0.0) * 15.0 / 120.0);
                if amt != 0.0 {
                    f = f.value(ax, amt);
                    if let Some(d) = event.amount_v120(ax) {
                        f = f.v120(ax, d as i32);
                    }
                } else if src == AxisSource::Finger {
                    f = f.stop(ax);
                }
            }
            let ptr = state.seat.get_pointer().unwrap();
            ptr.axis(state, f);
            ptr.frame(state);
        }
        _ => {}
    }
}

pub fn pointer_button(state: &mut State, button: u32, bs: ButtonState, time: u32) {
    state.last_input = Instant::now();
    activity(state);
    if state.ui.is_some() {
        if bs == ButtonState::Pressed {
            ui_click(state);
        }
        return; // the login / lock card: nothing else sees the click
    }
    if !state.pointer_live {
        return;
    }
    if state.cursor_typing_hidden {
        state.cursor_typing_hidden = false;
        state.dirty = true;
    }
    let serial = SERIAL_COUNTER.next_serial();
    let ptr = state.seat.get_pointer().unwrap();
    if bs == ButtonState::Pressed && !ptr.is_grabbed() {
        let u = under(state, state.pointer_loc);
        // a click outside an open flyout (start, widgets, quick settings) closes it
        let on_flyout = matches!(&u, Under::Surface(_, _, Some(l)) if l.namespace().starts_with("shadow-flyout"));
        if !on_flyout && flyout_open(state) {
            tell_panel("hide");
        }
        match u {
            Under::Tutorial(b) => {
                match b {
                    "skip" => if let Some(t) = state.tutorial.as_mut() { t.skip() },
                    _ => close_tutorial(state),
                }
                state.dirty = true;
                return; // the panel's own click: no client sees it
            }
            Under::Node(id) => {
                state.focus_window(id);
                return;
            }
            Under::Title(id, b) => {
                title_click(state, id, b);
                return; // the compositor's own bar: no client sees it
            }
            Under::Surface(s, _, layer) => click_focus(state, &s, layer),
            Under::Nothing => {
                // an empty spot of a screen: that screen gets the keyboard
                if let Some(si) = state.desk.screen_at(state.pointer_loc.x, state.pointer_loc.y) {
                    if si != state.desk.focused {
                        state.desk.focused = si;
                        state.cur = si;
                        state.relayout();
                    }
                }
            }
        }
    }
    ptr.button(state, &ButtonEvent { button, state: bs, serial, time });
    ptr.frame(state);
}

/// Show workspace `to` on screen `si`: the windows slide in from the side they come from.
pub fn switch_workspace(state: &mut State, si: usize, to: usize) {
    let from = state.desk.screens.get(si).map(|s| s.active).unwrap_or(0);
    let w = state.desk.screens.get(si).map(|s| s.size.0).unwrap_or(1280.0);
    if !state.desk.switch(si, to) {
        return;
    }
    state.desk.focused = si;
    state.cur = si;
    // where the arriving windows start: one screen-width to the side
    let dx = if to > from { w } else { -w };
    let targets = state.targets();
    let start: std::collections::HashMap<u64, crate::orbit::R> = targets.iter().map(|(id, r, _, _)| (*id, crate::orbit::R { x: r.x + dx, ..*r })).collect();
    let ms = state.cfg.layout.anim_ms.max(1) * 3 / 2;
    state.anim = (state.cfg.layout.anim_ms > 0).then(|| crate::state::Anim::Move { start: Instant::now(), dur: Duration::from_millis(ms), from: start });
    state.relayout();
    state.on_focus_changed();
}

/// is one of the shell's flyouts mapped on any screen?
fn flyout_open(state: &State) -> bool {
    state.desk.screens.iter().any(|s| layer_map_for_output(&s.out).layers().any(|l| l.namespace().starts_with("shadow-flyout")))
}

/// one line to shadow-panel's socket (never blocks the compositor)
pub fn tell_panel(line: &str) {
    let p = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join("shadow-panel.sock");
    if let Ok(mut s) = std::os::unix::net::UnixStream::connect(p) {
        use std::io::Write;
        let _ = s.set_nonblocking(true);
        let _ = s.write_all(format!("{line}\n").as_bytes());
    }
}

/// a click on a title bar: focus, or one of the buttons; a double click on the bar maximises
fn title_click(state: &mut State, id: u64, b: crate::orbit::TitleBtn) {
    use crate::orbit::TitleBtn::*;
    let double = state.last_title_click.is_some_and(|(i, t)| i == id && t.elapsed() < Duration::from_millis(400));
    state.last_title_click = Some((id, Instant::now()));
    state.focus_window(id);
    match b {
        Min => state.minimize(id),
        Max => toggle_max(state, id),
        Close => state.close_center(),
        Bar if double => {
            state.last_title_click = None;
            toggle_max(state, id);
        }
        Bar => {}
    }
    state.dirty = true;
}

pub fn toggle_max(state: &mut State, id: u64) {
    state.transition(|_| {});
    let others: Vec<u64> = state.ring().order.clone();
    for w in state.wins.iter_mut() {
        if w.id == id {
            w.size.max = !w.size.max;
            w.size.full = false;
        } else if others.contains(&w.id) {
            w.size.max = false; // one maximised window per workspace
        }
    }
    state.relayout();
}

/// test hook (SHADOW_TEST_INPUT only): move to x,y and click there, through the same paths
pub fn test_click(state: &mut State, x: f64, y: f64) {
    pointer_to(state, (x, y).into(), 0);
    pointer_button(state, 0x110, ButtonState::Pressed, 0);
    pointer_button(state, 0x110, ButtonState::Released, 0);
}

/// a click moves the keyboard: into a layer surface that accepts it (hub widgets, a menu), or
/// back to the centre window
fn click_focus(state: &mut State, s: &WlSurface, layer: Option<LayerSurface>) {
    let root = {
        let mut r = s.clone();
        while let Some(p) = smithay::wayland::compositor::get_parent(&r) {
            r = p;
        }
        r
    };
    match layer {
        Some(l) if l.can_receive_keyboard_focus() => {
            if state.layer_kb_focus.as_ref() != Some(l.wl_surface()) {
                state.layer_kb_focus = Some(l.wl_surface().clone());
                state.relayout();
            }
        }
        Some(_) => {}
        None => {
            let id = state.wins.iter().find(|w| w.window.toplevel().map(|t| t.wl_surface() == &root).unwrap_or(false)).map(|w| w.id);
            if let Some(id) = id {
                let focused_here = state.desk.locate(id).map(|l| l.0) == Some(state.desk.focused) && state.ring().center() == Some(id);
                if !focused_here {
                    state.focus_window(id);
                }
            }
            if state.layer_kb_focus.is_some() {
                state.layer_kb_focus = None;
                state.relayout();
            }
        }
    }
}

fn pointer_to(state: &mut State, p: Point<f64, Logical>, time: u32) {
    let (cx, cy) = if state.desk.screens.is_empty() {
        let (w, h) = state.output_size();
        (p.x.clamp(0.0, w - 1.0), p.y.clamp(0.0, h - 1.0))
    } else {
        state.desk.clamp(p.x, p.y)
    };
    state.pointer_loc = (cx, cy).into();
    activity(state);
    if state.ui.is_some() {
        state.cursor_typing_hidden = false;
        state.dirty = true; // the card's hover and its cursor
        return;
    }
    if !state.pointer_live {
        return;
    }
    state.last_input = Instant::now();
    state.cursor_typing_hidden = false;
    let u = under(state, state.pointer_loc);
    let hover_tut = matches!(u, Under::Tutorial(_));
    let focus = surface_focus(&u);
    // nothing of a client's under the pointer: the compositor's own arrow
    if focus.is_none() {
        let want = if hover_tut || matches!(u, Under::Node(_) | Under::Title(..)) { smithay::input::pointer::CursorIcon::Pointer } else { smithay::input::pointer::CursorIcon::Default };
        if !matches!(&state.cursor_status, smithay::input::pointer::CursorImageStatus::Named(i) if *i == want) {
            state.cursor_status = smithay::input::pointer::CursorImageStatus::Named(want);
        }
    }
    // pointer constraints follow the pointer focus: a lock is released when its surface loses
    // the pointer (Mod+Tab, a click elsewhere) and granted when it gets it back
    let ptr = state.seat.get_pointer().unwrap();
    let fs = focus.as_ref().map(|f| f.0.clone());
    if state.constrained.is_some() && state.constrained != fs {
        if let Some(old) = state.constrained.take() {
            smithay::wayland::pointer_constraints::with_pointer_constraint(&old, &ptr, |c| if let Some(c) = c { c.deactivate() });
        }
    }
    if let Some(s) = fs.as_ref().filter(|_| state.constrained.is_none()) {
        let got = smithay::wayland::pointer_constraints::with_pointer_constraint(s, &ptr, |c| c.map(|c| { if !c.is_active() { c.activate() }; true }).unwrap_or(false));
        if got {
            state.constrained = Some(s.clone());
        }
    }
    ptr.motion(state, focus, &MotionEvent { location: state.pointer_loc, serial: SERIAL_COUNTER.next_serial(), time });
    ptr.frame(state);
    state.dirty = true;
}

#[cfg(test)]
mod t {
    #[test]
    fn binds() {
        let (m, s) = super::parse_bind("Mod+Shift+Tab", "super").unwrap();
        assert!(m.logo && m.shift && !m.alt);
        assert_eq!(s, smithay::input::keyboard::Keysym::Tab);
        let (m, _) = super::parse_bind("Mod+BracketRight", "alt").unwrap();
        assert!(m.alt && !m.logo);
        assert!(super::parse_bind("Mod+NoSuchKey", "super").is_none());
        assert!(super::parse_bind("Hyper+X", "super").is_none());
        assert!(super::parse_bind("XF86AudioMute", "super").is_some());
    }
}
