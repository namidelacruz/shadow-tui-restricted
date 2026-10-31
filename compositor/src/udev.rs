//! Native backend: DRM/KMS + GBM + EGL/GLES on the primary GPU, libinput through libseat.
//! One active output (the built-in panel unless display.output names another connector).
//! Rendering is damage-driven: a frame is queued only when the scene changed, so an idle desktop
//! issues no page flips and the panel can enter Panel Self-Refresh.
use crate::render::El;
use crate::state::{Backend, State};
use smithay::{
    backend::{
        allocator::{
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
            Fourcc,
        },
        drm::{
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode,
        },
        egl::{EGLContext, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{gles::GlesRenderer, ImportDma, ImportMemWl},
        session::{libseat::LibSeatSession, Event as SessionEvent, Session},
        udev::{all_gpus, primary_gpu},
    },
    output::{Mode as WlMode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::EventLoop,
        drm::control::{connector, Device as _, Mode, ModeTypeFlags},
        input::Libinput,
        rustix::fs::OFlags,
    },
    utils::{DeviceFd, Transform},
};

type Out = DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;
type Om = DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

pub struct UdevData {
    pub session: LibSeatSession,
    pub renderer: Option<GlesRenderer>,
    out: Option<Out>,
    om: Option<Om>,
    modes: Vec<Mode>,
    libinput: Libinput,
    pending: bool,
    active: bool,
}

impl UdevData {
    pub fn change_vt(&mut self, vt: i32) {
        if let Err(e) = self.session.change_vt(vt) {
            tracing::warn!("vt switch: {e}");
        }
    }
}

fn pick_mode(modes: &[Mode], hz: u32) -> Option<Mode> {
    let pref = modes.iter().find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED)).or_else(|| modes.first())?;
    let size = pref.size();
    modes.iter().filter(|m| m.size() == size).min_by_key(|m| (m.vrefresh() as i64 - hz as i64).abs()).copied()
}

pub fn init(event_loop: &mut EventLoop<'static, State>, state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, notifier) = LibSeatSession::new()?;
    let seat = session.seat();
    let gpu = std::env::var("SHADOW_DRM_DEVICE").ok().map(std::path::PathBuf::from)
        .or_else(|| primary_gpu(&seat).ok().flatten())
        .or_else(|| all_gpus(&seat).ok().and_then(|v| v.into_iter().next()))
        .ok_or("no GPU found")?;
    tracing::info!("GPU {}", gpu.display());
    let fd = session.open(&gpu, OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK)?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let node = DrmNode::from_file(&fd).ok();
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;
    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let ctx = EGLContext::new(&display)?;
    let render_formats = ctx.dmabuf_render_formats().clone();
    let mut renderer = unsafe { GlesRenderer::new(ctx)? };
    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), node);
    let mut om: Om = DrmOutputManager::new(drm, allocator, exporter, Some(gbm), [Fourcc::Argb8888, Fourcc::Xrgb8888, Fourcc::Abgr8888], render_formats);

    // connector: configured name, else the built-in panel, else the first connected one
    let dev = om.device();
    let res = dev.resource_handles()?;
    let mut conns: Vec<connector::Info> = res.connectors().iter().filter_map(|c| dev.get_connector(*c, false).ok()).filter(|i| i.state() == connector::State::Connected).collect();
    let name = |i: &connector::Info| format!("{}-{}", i.interface().as_str(), i.interface_id());
    let want = state.cfg.display.output.clone();
    conns.sort_by_key(|i| {
        let n = name(i);
        (if !want.is_empty() && n == want { 0 } else if n.starts_with("eDP") { 1 } else { 2 }, n)
    });
    let conn = conns.into_iter().next().ok_or("no connected display")?;
    let crtc = conn.encoders().iter().filter_map(|e| dev.get_encoder(*e).ok()).flat_map(|e| res.filter_crtcs(e.possible_crtcs())).next().ok_or("no free CRTC")?;
    let modes = conn.modes().to_vec();
    let pw = crate::hud::power();
    let hz = crate::hud::wanted_refresh().unwrap_or(if pw.ac { state.cfg.display.refresh_ac } else { state.cfg.display.refresh_battery });
    let mode = pick_mode(&modes, hz).ok_or("connector has no modes")?;
    let (pw_mm, ph_mm) = conn.size().unwrap_or((0, 0));
    let output = Output::new(name(&conn), PhysicalProperties { size: (pw_mm as i32, ph_mm as i32).into(), subpixel: Subpixel::Unknown, make: "Shadow".into(), model: name(&conn) });
    let _ = output.create_global::<State>(&state.dh);
    let scale = crate::winit::scale_override(state.cfg.look.scale);
    output.change_current_state(Some(WlMode::from(mode)), Some(Transform::Normal), Some(Scale::Fractional(scale)), Some((0, 0).into()));
    output.set_preferred(WlMode::from(mode));
    state.space.map_output(&output, (0, 0));
    state.output = Some(output.clone());
    state.refresh_hz = mode.vrefresh();
    let out = om.initialize_output::<_, El>(crtc, mode, &[conn.handle()], &output, None, &mut renderer, &DrmOutputRenderElements::default())
        .map_err(|e| format!("initialise output: {e:?}"))?;
    state.shm_state.update_formats(renderer.shm_formats());
    state.dmabuf_global = Some(state.dmabuf_state.create_global::<State>(&state.dh, renderer.dmabuf_formats()));

    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput.udev_assign_seat(&seat).map_err(|_| "libinput seat")?;
    let li_backend = LibinputInputBackend::new(libinput.clone());
    event_loop.handle().insert_source(li_backend, |e, _, st| crate::input::on_input(st, e))?;

    event_loop.handle().insert_source(notifier, |ev, _, st| {
        if let Backend::Udev(u) = &mut st.backend {
            match ev {
                SessionEvent::PauseSession => {
                    u.libinput.suspend();
                    if let Some(om) = u.om.as_mut() {
                        om.pause();
                    }
                    u.active = false;
                }
                SessionEvent::ActivateSession => {
                    let _ = u.libinput.resume();
                    if let Some(om) = u.om.as_mut() {
                        let _ = om.activate(false);
                    }
                    u.active = true;
                    u.pending = false;
                    st.dirty = true;
                }
            }
        }
    })?;
    event_loop.handle().insert_source(drm_notifier, |ev, _, st| {
        if let DrmEvent::VBlank(_) = ev {
            if let Backend::Udev(u) = &mut st.backend {
                if let Some(o) = u.out.as_mut() {
                    let _ = o.frame_submitted();
                }
                u.pending = false;
            }
        }
    })?;

    state.backend = Backend::Udev(UdevData { session, renderer: Some(renderer), out: Some(out), om: Some(om), modes, libinput, pending: false, active: true });
    Ok(())
}

pub fn render(state: &mut State) {
    let Backend::Udev(mut u) = std::mem::replace(&mut state.backend, Backend::None) else { return };
    if u.active && !u.pending {
        if let Some(ui) = state.ui.as_mut() {
            ui.tick();
        }
        state.dirty = false;
        if let (Some(r), Some(o)) = (u.renderer.as_mut(), u.out.as_mut()) {
            let els = state.elements(r);
            match o.render_frame(r, &els, [0.0, 0.0, 0.0, 1.0], FrameFlags::DEFAULT) {
                Ok(res) if !res.is_empty => match o.queue_frame(()) {
                    Ok(()) => u.pending = true,
                    Err(e) => tracing::warn!("queue frame: {e:?}"),
                },
                Ok(_) => {}
                Err(e) => tracing::warn!("render frame: {e:?}"),
            }
        }
    }
    state.backend = Backend::Udev(u);
    if let Some(o) = state.output.clone() {
        state.send_frames(&o);
    }
    state.space.refresh();
    state.popups.cleanup();
}

pub fn schedule(state: &mut State) {
    let animating = state.animating();
    if state.dirty || animating {
        render(state);
    }
}

/// switch the panel between the AC and battery refresh rates (DRM atomic mode change)
pub fn apply_refresh(state: &mut State, hz: u32) {
    let Backend::Udev(mut u) = std::mem::replace(&mut state.backend, Backend::None) else { return };
    if let (Some(mode), Some(r), Some(o)) = (pick_mode(&u.modes, hz), u.renderer.as_mut(), u.out.as_mut()) {
        if mode.vrefresh() != state.refresh_hz {
            match o.use_mode::<_, El>(mode, r, &DrmOutputRenderElements::default()) {
                Ok(()) => {
                    if let Some(out) = state.output.as_ref() {
                        out.change_current_state(Some(WlMode::from(mode)), None, None, None);
                    }
                    state.refresh_hz = mode.vrefresh();
                    tracing::info!("refresh rate → {} Hz", state.refresh_hz);
                }
                Err(e) => tracing::warn!("mode change to {hz} Hz failed: {e:?}"),
            }
        }
    }
    state.backend = Backend::Udev(u);
    state.dirty = true;
}
