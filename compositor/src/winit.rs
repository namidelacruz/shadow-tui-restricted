//! Nested backend: runs as a window inside another Wayland/X11 session (development and testing).
use crate::state::{Backend, State};
use smithay::{
    backend::{
        renderer::{damage::OutputDamageTracker, gles::GlesRenderer, ImportDma, ImportMemWl},
        winit::{self, WinitEvent, WinitGraphicsBackend},
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::calloop::EventLoop,
    utils::Transform,
};

pub struct WinitData {
    pub backend: WinitGraphicsBackend<GlesRenderer>,
    pub damage: OutputDamageTracker,
}

pub fn scale_override(cfg_scale: f64) -> f64 {
    std::env::var("SHADOW_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(cfg_scale)
}

pub fn init(event_loop: &mut EventLoop<'static, State>, state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    let mode = Mode { size: backend.window_size(), refresh: 60_000 };
    let output = Output::new("winit".into(), PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "Shadow".into(), model: "Nested".into() });
    let _ = output.create_global::<State>(&state.dh);
    let scale = scale_override(1.0);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), Some(Scale::Fractional(scale)), Some((0, 0).into()));
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));
    state.output = Some(output.clone());
    state.refresh_hz = 60;
    state.shm_state.update_formats(backend.renderer().shm_formats());
    let formats = backend.renderer().dmabuf_formats();
    state.dmabuf_global = Some(state.dmabuf_state.create_global::<State>(&state.dh, formats));
    let damage = OutputDamageTracker::from_output(&output);
    state.backend = Backend::Winit(WinitData { backend, damage });

    event_loop.handle().insert_source(winit, move |event, _, state| match event {
        WinitEvent::Resized { size, .. } => {
            if let Some(o) = state.output.clone() {
                o.change_current_state(Some(Mode { size, refresh: 60_000 }), None, None, None);
                state.space.map_output(&o, (0, 0));
                if let Some(o) = state.output.as_ref() {
                    smithay::desktop::layer_map_for_output(o).arrange();
                }
            }
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
    let age = w.backend.buffer_age().unwrap_or(0);
    let result = match w.backend.bind() {
        Ok((renderer, mut fb)) => {
            let els = state.elements(renderer);
            w.damage.render_output(renderer, &mut fb, age, &els, [0.0, 0.0, 0.0, 1.0]).map(|r| r.damage.cloned())
                .map_err(|e| format!("{e:?}"))
        }
        Err(e) => Err(format!("{e:?}")),
    };
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
    if let Some(o) = state.output.clone() {
        state.send_frames(&o);
    }
    state.space.refresh();
    state.popups.cleanup();
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
