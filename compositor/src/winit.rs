//! Nested backend: runs as a window inside another Wayland/X11 session (development and testing).
//!
//! `SHADOW_SPLIT_OUTPUTS=2` splits the window into two side-by-side screens ("winit-1" on the
//! left as the built-in panel, "winit-2" as the external monitor), so multi-monitor behaviour —
//! apps opening on the second screen, per-screen workspaces, moving windows between screens — can
//! be tried and tested without a second monitor.
use crate::state::{Backend, State};
use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker,
            element::{Element, RenderElement},
            gles::GlesRenderer,
            Frame, ImportDma, ImportMemWl, Renderer,
        },
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Physical, Point, Rectangle, Size, Transform},
};

pub struct WinitData {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub damage: OutputDamageTracker,
    /// screens the window is split into (1 = the whole window)
    pub split: usize,
}

pub fn scale_override(cfg_scale: f64) -> f64 {
    std::env::var("SHADOW_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(cfg_scale)
}

fn split_count() -> usize {
    std::env::var("SHADOW_SPLIT_OUTPUTS").ok().and_then(|v| v.parse().ok()).unwrap_or(1).clamp(1, 4)
}

/// the window's physical size cut into `n` columns
fn parts(size: Size<i32, Physical>, n: usize) -> Vec<Size<i32, Physical>> {
    let w = size.w / n as i32;
    (0..n).map(|i| Size::from((if i + 1 == n { size.w - w * (n as i32 - 1) } else { w }, size.h))).collect()
}

pub fn init(event_loop: &mut EventLoop<'static, State>, state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    let n = split_count();
    let scale = scale_override(1.0);
    let sizes = parts(backend.window_size(), n);
    let mut first: Option<Output> = None;
    for (i, sz) in sizes.iter().enumerate() {
        // split mode: the first part plays the laptop panel
        let name = if n == 1 { "winit".to_string() } else if i == 0 { "eDP-winit".into() } else { format!("winit-{}", i + 1) };
        let output = Output::new(name, PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "Shadow".into(), model: "Nested".into() });
        let _ = output.create_global::<State>(&state.dh);
        let mode = Mode { size: *sz, refresh: 60_000 };
        output.change_current_state(Some(mode), Some(Transform::Flipped180), Some(Scale::Fractional(scale)), Some((0, 0).into()));
        output.set_preferred(mode);
        first.get_or_insert_with(|| output.clone());
        state.add_screen(output);
    }
    state.refresh_hz = 60;
    state.shm_state.update_formats(backend.renderer().shm_formats());
    let formats = backend.renderer().dmabuf_formats();
    state.dmabuf_global = Some(state.dmabuf_state.create_global::<State>(&state.dh, formats));
    let damage = OutputDamageTracker::from_output(first.as_ref().unwrap());
    // the compositor draws its own cursor: hide the host's over the window
    backend.window().set_cursor_visible(false);
    state.backend = Backend::Winit(WinitData { backend, damage, split: n });

    event_loop.handle().insert_source(winit, move |event, _, state| match event {
        WinitEvent::Resized { size, .. } => {
            let outs: Vec<Output> = state.desk.screens.iter().map(|s| s.out.clone()).collect();
            for (o, sz) in outs.iter().zip(parts(size, outs.len().max(1))) {
                o.change_current_state(Some(Mode { size: sz, refresh: 60_000 }), None, None, None);
            }
            state.place_screens();
            state.relayout();
        }
        WinitEvent::Input(e) => crate::input::on_input(state, e),
        WinitEvent::Redraw => render(state),
        WinitEvent::CloseRequested => state.signal.stop(),
        _ => {}
    })?;
    Ok(())
}

pub fn render(state: &mut State) {
    let Backend::Winit(mut w) = std::mem::replace(&mut state.backend, Backend::None) else { return };
    if let Some(u) = state.ui.as_mut() {
        u.tick();
    }
    state.dirty = false;
    let result = if w.split > 1 { render_split(state, &mut w) } else { render_one(state, &mut w) };
    match result {
        Ok(Some(d)) => {
            if let Err(e) = w.backend.submit(Some(&d)) {
                tracing::warn!("submit: {e}");
            }
        }
        Ok(None) => {}
        Err(e) => tracing::warn!("render: {e}"),
    }
    state.backend = Backend::Winit(w);
    for i in 0..state.desk.screens.len() {
        let o = state.desk.screens[i].out.clone();
        state.with_screen(i, |st| st.send_frames(&o));
    }
    state.space.refresh();
    state.popups.cleanup();
}

fn render_one(state: &mut State, w: &mut WinitData) -> Result<Option<Vec<Rectangle<i32, Physical>>>, String> {
    let age = w.backend.buffer_age().unwrap_or(0);
    let (renderer, mut fb) = w.backend.bind().map_err(|e| format!("{e:?}"))?;
    let els = state.with_screen(0, |st| st.elements(renderer));
    w.damage.render_output(renderer, &mut fb, age, &els, [0.0, 0.0, 0.0, 1.0]).map(|r| r.damage.cloned()).map_err(|e| format!("{e:?}"))
}

/// test mode: every screen drawn into its column of the window (no damage tracking: full frames)
fn render_split(state: &mut State, w: &mut WinitData) -> Result<Option<Vec<Rectangle<i32, Physical>>>, String> {
    let size = w.backend.window_size();
    let scale = state.desk.screens.first().map(|s| s.out.current_scale().fractional_scale()).unwrap_or(1.0);
    let n = state.desk.screens.len();
    let (renderer, mut fb) = w.backend.bind().map_err(|e| format!("{e:?}"))?;
    let lists: Vec<Vec<crate::render::El>> = (0..n).map(|i| state.with_screen(i, |st| st.elements(renderer))).collect();
    let offsets: Vec<i32> = state.desk.screens.iter().map(|s| (s.loc.0 * scale).round() as i32).collect();
    let full = Rectangle::from_size(size);
    let mut frame = renderer.render(&mut fb, size, Transform::Flipped180).map_err(|e| format!("{e:?}"))?;
    frame.clear([0.0, 0.0, 0.0, 1.0].into(), &[full]).map_err(|e| format!("{e:?}"))?;
    for (els, dx) in lists.iter().zip(offsets) {
        // the list is front to back: draw back to front
        for e in els.iter().rev() {
            let g = e.geometry(smithay::utils::Scale::from(scale));
            let dst = Rectangle::new(g.loc + Point::from((dx, 0)), g.size);
            let damage = [Rectangle::from_size(dst.size)];
            if let Err(err) = RenderElement::<GlesRenderer>::draw(e, &mut frame, e.src(), dst, &damage, &[]) {
                tracing::warn!("split draw: {err:?}");
            }
        }
    }
    let _ = frame.finish().map_err(|e| format!("{e:?}"))?;
    Ok(Some(vec![full]))
}

/// called after every dispatch: ask the host for a frame only if something changed
pub fn schedule(state: &mut State) {
    let animating = state.animating();
    if state.dirty || animating {
        if let Backend::Winit(w) = &state.backend {
            w.backend.window().request_redraw();
        }
    }
}
