//! Keyboard-first input. Keys go through the bind table, then to the centre-stage client.
//! The pointer is masked: motion/buttons/scroll are dropped unless the centre window is tagged
//! requires_pointer (config `input.pointer_apps` or a Lua rule).
use crate::state::State;
use crate::ui::{Mode, Ui, UiAction, UiMsg};
use smithay::{
    backend::input::{AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent},
    input::{
        keyboard::{keysyms, xkb, FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
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
    state.dirty = true;
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

pub fn run_action(state: &mut State, a: &str) {
    let (verb, rest) = a.split_once(' ').unwrap_or((a, ""));
    match verb {
        "spawn" => spawn(state, rest),
        "focus-next" => state.transition(|r| r.next()),
        "focus-prev" => state.transition(|r| r.prev()),
        "width-up" | "width-down" | "fullscreen" => {
            let Some(id) = state.ring.center() else { return };
            let n = state.cfg.layout.presets.len();
            state.transition(|_| {});
            if let Some(w) = state.wins.iter_mut().find(|w| w.id == id) {
                match verb {
                    "width-up" => w.size.step(true, n),
                    "width-down" => w.size.step(false, n),
                    _ => w.size.toggle_full(),
                }
            }
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
        "quit" => state.signal.stop(),
        "lua" => {
            if let (Some(l), Ok(i)) = (state.lua.as_mut(), rest.parse::<usize>()) {
                l.call_bind(i);
            }
            state.run_lua_actions();
        }
        _ => tracing::warn!("unknown action: {a}"),
    }
}

// ---------------------------------------------------------------- pointer (masked)
pub fn on_input<B: InputBackend>(state: &mut State, event: InputEvent<B>) {
    match event {
        InputEvent::Keyboard { event } => on_key(state, event.key_code(), event.state(), event.time_msec()),
        InputEvent::PointerMotion { event } => {
            let d = event.delta();
            let p = state.pointer_loc + d;
            pointer_to(state, p, event.time_msec());
        }
        InputEvent::PointerMotionAbsolute { event } => {
            let (w, h) = state.output_size();
            let p = event.position_transformed((w as i32, h as i32).into());
            pointer_to(state, p, event.time_msec());
        }
        InputEvent::PointerButton { event } => {
            state.last_input = Instant::now();
            if !state.pointer_live {
                return; // keyboard-first: buttons are ignored for keyboard-only windows
            }
            let ptr = state.seat.get_pointer().unwrap();
            ptr.button(state, &ButtonEvent { button: event.button_code(), state: event.state(), serial: SERIAL_COUNTER.next_serial(), time: event.time_msec() });
            ptr.frame(state);
            let _ = ButtonState::Pressed;
        }
        InputEvent::PointerAxis { event } => {
            if !state.pointer_live {
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

fn pointer_to(state: &mut State, p: Point<f64, Logical>, time: u32) {
    let (w, h) = state.output_size();
    state.pointer_loc = (p.x.clamp(0.0, w - 1.0), p.y.clamp(0.0, h - 1.0)).into();
    if !state.pointer_live {
        return; // masked: no client sees the motion and nothing is redrawn
    }
    state.last_input = Instant::now();
    let under = state.ring.center().and_then(|id| state.win(id)).and_then(|w| {
        let r = state.center_rect(w.id);
        let loc = Point::<f64, Logical>::from((r.x, r.y)) - w.window.geometry().loc.to_f64();
        w.window.surface_under(state.pointer_loc - loc, smithay::desktop::WindowSurfaceType::ALL).map(|(s, sp)| (s, (sp.to_f64() + loc)))
    });
    let ptr = state.seat.get_pointer().unwrap();
    ptr.motion(state, under, &MotionEvent { location: state.pointer_loc, serial: SERIAL_COUNTER.next_serial(), time });
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
