//! Native backend: DRM/KMS + GBM + EGL/GLES on the primary GPU, libinput through libseat.
//! 10.0: every connected monitor is a screen (`[display] all_outputs = false` keeps only the
//! preferred one), and monitors plugged in or out while running are added / removed (udev).
//! Rendering is damage-driven and per screen: a frame is queued only when that screen changed, so
//! an idle desktop issues no page flips and the panel can enter Panel Self-Refresh.
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
        udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent},
    },
    output::{Mode as WlMode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::EventLoop,
        drm::control::{connector, crtc, Device as _, Mode, ModeTypeFlags},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::backend::GlobalId,
    },
    utils::{DeviceFd, Transform},
};

type Out = DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;
type Om = DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// one lit monitor
struct Head {
    output: Output,
    global: GlobalId,
    drm: Out,
    crtc: crtc::Handle,
    conn: connector::Handle,
    modes: Vec<Mode>,
    /// a frame is queued and not yet shown
    pending: bool,
}

pub struct UdevData {
    pub session: LibSeatSession,
    pub renderer: Option<GlesRenderer>,
    heads: Vec<Head>,
    om: Option<Om>,
    libinput: Libinput,
    active: bool,
    dev_id: Option<u64>,
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

fn conn_name(i: &connector::Info) -> String {
    format!("{}-{}", i.interface().as_str(), i.interface_id())
}

fn internal(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n.starts_with("EDP") || n.starts_with("LVDS") || n.starts_with("DSI")
}

/// the panel's wanted refresh (AC / battery / shadow-powerd); external monitors use their preferred
fn wanted_hz(state: &State, name: &str) -> u32 {
    if !internal(name) {
        return 0;
    }
    let pw = crate::hud::power();
    crate::hud::wanted_refresh().unwrap_or(if pw.ac { state.cfg.display.refresh_ac } else { state.cfg.display.refresh_battery })
}

/// Light up every connected connector that isn't yet (and drop heads whose monitor is gone).
fn scan(state: &mut State, u: &mut UdevData) {
    let Some(om) = u.om.as_mut() else { return };
    let Some(renderer) = u.renderer.as_mut() else { return };
    let dev = om.device();
    let Ok(res) = dev.resource_handles() else { return };
    let mut conns: Vec<connector::Info> = res.connectors().iter().filter_map(|c| dev.get_connector(*c, false).ok()).collect();
    // gone: drop the head (frees its CRTC), the screen's windows move to the primary
    let connected: Vec<connector::Handle> = conns.iter().filter(|i| i.state() == connector::State::Connected).map(|i| i.handle()).collect();
    let mut i = 0;
    while i < u.heads.len() {
        if !connected.contains(&u.heads[i].conn) {
            let h = u.heads.remove(i);
            tracing::info!("monitor {} disconnected", h.output.name());
            state.remove_screen(&h.output.name());
            state.dh.remove_global::<State>(h.global.clone());
            drop(h);
        } else {
            i += 1;
        }
    }
    // new: configured name, else the built-in panel, else the rest by name
    let want = state.cfg.display.output.clone();
    conns.retain(|i| i.state() == connector::State::Connected && !u.heads.iter().any(|h| h.conn == i.handle()));
    conns.sort_by_key(|i| {
        let n = conn_name(i);
        (if !want.is_empty() && n == want { 0 } else if internal(&n) { 1 } else { 2 }, n)
    });
    if !state.cfg.display.all_outputs && !u.heads.is_empty() {
        return;
    }
    for conn in conns {
        if !state.cfg.display.all_outputs && !u.heads.is_empty() {
            break;
        }
        let used: Vec<crtc::Handle> = u.heads.iter().map(|h| h.crtc).collect();
        let dev = om.device();
        let Some(crtc) = conn.encoders().iter().filter_map(|e| dev.get_encoder(*e).ok()).flat_map(|e| res.filter_crtcs(e.possible_crtcs())).find(|c| !used.contains(c)) else {
            tracing::warn!("{}: no free CRTC (the GPU drives fewer displays)", conn_name(&conn));
            continue;
        };
        let modes = conn.modes().to_vec();
        let name = conn_name(&conn);
        let Some(mode) = pick_mode(&modes, wanted_hz(state, &name)) else { continue };
        let (pw_mm, ph_mm) = conn.size().unwrap_or((0, 0));
        let output = Output::new(name.clone(), PhysicalProperties { size: (pw_mm as i32, ph_mm as i32).into(), subpixel: Subpixel::Unknown, make: "Shadow".into(), model: name.clone() });
        let global = output.create_global::<State>(&state.dh);
        // the panel keeps its configured scale; another monitor starts at a scale from its size
        let scale = if internal(&name) || u.heads.is_empty() { crate::winit::scale_override(state.cfg.look.scale) } else { external_scale(mode.size(), (pw_mm, ph_mm)) };
        output.change_current_state(Some(WlMode::from(mode)), Some(Transform::Normal), Some(Scale::Fractional(scale)), Some((0, 0).into()));
        output.set_preferred(WlMode::from(mode));
        match om.initialize_output::<_, El>(crtc, mode, &[conn.handle()], &output, None, renderer, &DrmOutputRenderElements::default()) {
            Ok(drm) => {
                tracing::info!("monitor {name}: {}x{}@{} scale {scale}", mode.size().0, mode.size().1, mode.vrefresh());
                if internal(&name) || u.heads.is_empty() {
                    state.refresh_hz = mode.vrefresh();
                }
                u.heads.push(Head { output: output.clone(), global, drm, crtc, conn: conn.handle(), modes, pending: false });
                state.add_screen(output);
            }
            Err(e) => {
                tracing::warn!("monitor {name}: cannot light it up: {e:?}");
                state.dh.remove_global::<State>(global);
            }
        }
    }
    state.dirty = true;
}

/// a sensible scale for a monitor: ~ 96 logical dpi, in quarter steps
fn external_scale(px: (u16, u16), mm: (u32, u32)) -> f64 {
    if mm.0 < 100 {
        return 1.0;
    }
    let dpi = px.0 as f64 / (mm.0 as f64 / 25.4);
    ((dpi / 110.0) * 4.0).round().clamp(4.0, 12.0) / 4.0
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
    let dev_id = node.map(|n| n.dev_id());
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let gbm = GbmDevice::new(fd.clone())?;
    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let ctx = EGLContext::new(&display)?;
    let render_formats = ctx.dmabuf_render_formats().clone();
    let renderer = unsafe { GlesRenderer::new(ctx)? };
    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), node);
    let om: Om = DrmOutputManager::new(drm, allocator, exporter, Some(gbm), [Fourcc::Argb8888, Fourcc::Xrgb8888, Fourcc::Abgr8888], render_formats);

    state.shm_state.update_formats(renderer.shm_formats());
    state.dmabuf_global = Some(state.dmabuf_state.create_global::<State>(&state.dh, renderer.dmabuf_formats()));

    let libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    let mut li = libinput.clone();
    li.udev_assign_seat(&seat).map_err(|_| "libinput seat")?;
    let mut u = UdevData { session, renderer: Some(renderer), heads: vec![], om: Some(om), libinput, active: true, dev_id };
    scan(state, &mut u);
    if u.heads.is_empty() {
        return Err("no connected display could be lit up".into());
    }

    let li_backend = LibinputInputBackend::new(li);
    event_loop.handle().insert_source(li_backend, |e, _, st| {
        if let smithay::backend::input::InputEvent::DeviceAdded { device } = &e {
            configure_device(&mut device.clone(), &st.cfg.input);
        }
        crate::input::on_input(st, e)
    })?;

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
                    for h in &mut u.heads {
                        h.pending = false;
                    }
                    st.dirty = true;
                }
            }
        }
    })?;
    event_loop.handle().insert_source(drm_notifier, |ev, _, st| {
        if let DrmEvent::VBlank(c) = ev {
            if let Backend::Udev(u) = &mut st.backend {
                if let Some(h) = u.heads.iter_mut().find(|h| h.crtc == c) {
                    let _ = h.drm.frame_submitted();
                    h.pending = false;
                }
            }
        }
    })?;
    // monitors plugged in / out
    match UdevBackend::new(&seat) {
        Ok(ub) => {
            event_loop.handle().insert_source(ub, |ev, _, st| {
                if let UdevEvent::Changed { device_id } = ev {
                    let Backend::Udev(mut u) = std::mem::replace(&mut st.backend, Backend::None) else { return };
                    if u.dev_id.is_none_or(|d| d == device_id) && u.active {
                        scan(st, &mut u);
                    }
                    st.backend = Backend::Udev(u);
                    st.relayout();
                }
            })?;
        }
        Err(e) => tracing::warn!("udev monitor: {e} (monitors plugged in later are not picked up)"),
    }

    state.backend = Backend::Udev(u);
    Ok(())
}

pub fn render(state: &mut State) {
    let Backend::Udev(mut u) = std::mem::replace(&mut state.backend, Backend::None) else { return };
    if u.active {
        if let Some(ui) = state.ui.as_mut() {
            ui.tick();
        }
        state.dirty = false;
        if let Some(r) = u.renderer.as_mut() {
            // a screen still waiting for its last flip is drawn after that vblank (stays dirty)
            let skipped = u.heads.iter().any(|h| h.pending);
            for h in u.heads.iter_mut().filter(|h| !h.pending) {
                let Some(si) = state.screen_of_output(&h.output) else { continue };
                let els = state.with_screen(si, |st| st.elements(r));
                match h.drm.render_frame(r, &els, [0.0, 0.0, 0.0, 1.0], FrameFlags::DEFAULT) {
                    Ok(res) if !res.is_empty => match h.drm.queue_frame(()) {
                        Ok(()) => h.pending = true,
                        Err(e) => tracing::warn!("queue frame ({}): {e:?}", h.output.name()),
                    },
                    Ok(_) => {}
                    Err(e) => tracing::warn!("render frame ({}): {e:?}", h.output.name()),
                }
            }
            if skipped {
                state.dirty = true;
            }
        }
    }
    let outs: Vec<Output> = u.heads.iter().map(|h| h.output.clone()).collect();
    state.backend = Backend::Udev(u);
    for o in outs {
        if let Some(si) = state.screen_of_output(&o) {
            state.with_screen(si, |st| st.send_frames(&o));
        }
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
    if let Some(r) = u.renderer.as_mut() {
        // the built-in panel (else the first monitor)
        let pos = u.heads.iter().position(|h| internal(&h.output.name())).or(if u.heads.is_empty() { None } else { Some(0) });
        if let Some(h) = pos.and_then(|i| u.heads.get_mut(i)) {
            if let Some(mode) = pick_mode(&h.modes, hz) {
                if mode.vrefresh() != state.refresh_hz {
                    match h.drm.use_mode::<_, El>(mode, r, &DrmOutputRenderElements::default()) {
                        Ok(()) => {
                            h.output.change_current_state(Some(WlMode::from(mode)), None, None, None);
                            state.refresh_hz = mode.vrefresh();
                            tracing::info!("refresh rate → {} Hz", state.refresh_hz);
                        }
                        Err(e) => tracing::warn!("mode change to {hz} Hz failed: {e:?}"),
                    }
                }
            }
        }
    }
    state.backend = Backend::Udev(u);
    state.dirty = true;
}

/// touchpads and mice as a laptop user expects them: tap to click, two-finger scroll,
/// disable-while-typing, middle-click emulation; natural scroll and speed from [input]
fn configure_device(d: &mut smithay::reexports::input::Device, cfg: &crate::config::Input) {
    if d.config_tap_finger_count() > 0 {
        let _ = d.config_tap_set_enabled(cfg.tap_to_click);
        let _ = d.config_tap_set_drag_enabled(true);
        let _ = d.config_dwt_set_enabled(true);
    }
    if d.config_scroll_has_natural_scroll() {
        let _ = d.config_scroll_set_natural_scroll_enabled(cfg.natural_scroll);
    }
    if d.config_accel_is_available() {
        let _ = d.config_accel_set_speed(cfg.pointer_speed.clamp(-1.0, 1.0));
    }
    if d.config_middle_emulation_is_available() {
        let _ = d.config_middle_emulation_set_enabled(true);
    }
    tracing::info!("input device: {} (tap {}, natural scroll {})", d.name(), cfg.tap_to_click, cfg.natural_scroll);
}
