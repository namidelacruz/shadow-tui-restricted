//! Wayland plumbing shared by every app: one event loop, one shm pool, any number of surfaces.
//! Surfaces are redrawn only when the app asks (`Ctx::redraw`) — there is no frame clock.
use crate::canvas::Canvas;
use crate::font::Fonts;
use crate::scale;
use crate::theme::Theme;
use smithay_client_toolkit::reexports::calloop::channel::{self, Channel};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, Dispatch, Proxy, QueueHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{cursor_shape::CursorShapeManager, PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT},
        Capability, SeatHandler, SeatState,
    },
    session_lock::{SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface, SessionLockSurfaceConfigure},
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        xdg::{
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use std::time::Duration;
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};

// ====================================================================== public API
/// Pointer events passed to `App::pointer`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Ptr {
    Move,
    Down,
    Up,
    Leave,
}

/// Keyboard focus for a layer surface: none (the bar), on demand, or exclusive (a flyout you
/// type into straight away).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LayerKeyboard {
    None,
    OnDemand,
    Exclusive,
}

pub enum SurfaceKind {
    /// A normal application window; `size` is the initial logical size.
    Window { app_id: String, title: String, size: (u32, u32) },
    /// A layer-shell panel (side bar). `size` 0 on an axis = stretch between anchors.
    Layer { namespace: String, layer: Layer, anchor: Anchor, size: (u32, u32), margin: (i32, i32, i32, i32), exclusive: i32, keyboard: LayerKeyboard },
    /// ext-session-lock: one full-screen surface per monitor until `Ctx::unlock`.
    Lock,
    /// Desktop background: one surface per monitor on the background layer, behind everything,
    /// following monitors as they come and go (`Frame::output` says which one is drawn).
    Background { namespace: String, keyboard: LayerKeyboard },
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutputDesc {
    pub name: String,
    pub make: String,
    pub model: String,
    /// current mode in device pixels
    pub px: (i32, i32),
    /// physical size from EDID
    pub mm: (i32, i32),
    pub refresh_mhz: i32,
}

impl OutputDesc {
    pub fn internal(&self) -> bool {
        scale::is_internal(&self.name)
    }
    pub fn ideal_scale(&self) -> Option<f64> {
        scale::ideal_scale(&self.name, self.px, self.mm)
    }
}

pub struct Key {
    pub sym: Keysym,
    pub text: Option<String>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    /// Caps Lock is on (password fields warn about it)
    pub caps: bool,
}

impl Key {
    /// Printable text typed (no Ctrl/Alt/Logo held).
    pub fn typed(&self) -> Option<&str> {
        if self.ctrl || self.alt || self.logo {
            return None;
        }
        self.text.as_deref().filter(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()))
    }
}

#[derive(Default)]
pub struct Ctx {
    redraw: bool,
    quit: bool,
    move_to: Option<Option<String>>,
    resize: Option<(u32, u32)>,
    unlock: bool,
    title: Option<String>,
    visible: Option<bool>,
    wake: bool,
}

impl Ctx {
    /// Repaint every surface once (the only way anything is drawn).
    pub fn redraw(&mut self) {
        self.redraw = true;
    }
    pub fn quit(&mut self) {
        self.quit = true;
    }
    /// Layer surfaces: move to the named output (None = let the compositor choose).
    pub fn move_to_output(&mut self, name: Option<String>) {
        self.move_to = Some(name);
    }
    /// Layer surfaces: request a new logical size.
    pub fn resize(&mut self, w: u32, h: u32) {
        self.resize = Some((w, h));
    }
    /// Lock surfaces: authentication succeeded, end the lock.
    pub fn unlock(&mut self) {
        self.unlock = true;
    }
    /// Layer surfaces: unmap (false) or map again (true).
    pub fn set_visible(&mut self, v: bool) {
        self.visible = Some(v);
    }
    /// Re-read `App::tick_ms` now and restart the timer from it (start an animation at once,
    /// instead of waiting out a long idle tick).
    pub fn wake(&mut self) {
        self.wake = true;
    }
    pub fn set_title(&mut self, t: &str) {
        self.title = Some(t.to_string());
    }
}

pub struct Frame<'a> {
    pub cv: Canvas<'a>,
    /// device pixels per logical pixel (the compositor's scale)
    pub scale: f32,
    /// device pixels per logical text pixel: scale x automatic text correction
    pub text: f32,
    pub fonts: &'a mut Fonts,
    pub theme: &'a Theme,
    pub output: Option<&'a str>,
    /// index of this surface (lock: one per output)
    pub index: usize,
    pub focused: bool,
    /// layer surfaces: set to request a new logical size (content-sized panels)
    pub want_size: Option<(u32, u32)>,
    /// logical size of this surface
    pub logical: (u32, u32),
}

impl Frame<'_> {
    /// logical length -> device pixels
    pub fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }
    /// line thickness: at least one device pixel
    pub fn hair(&self) -> i32 {
        (self.scale.floor() as i32).max(1)
    }
    /// font size in points -> pixels per em on this surface
    pub fn font_px(&self, pt: f32) -> f32 {
        (pt * 96.0 / 72.0 * self.text).round().max(4.0)
    }
    pub fn line_h(&mut self, pt: f32) -> i32 {
        let px = self.font_px(pt);
        self.fonts.metrics(px).1
    }
    pub fn text_w(&mut self, s: &str, pt: f32, bold: bool) -> i32 {
        let px = self.font_px(pt);
        self.fonts.width(s, px, bold)
    }
    pub fn fit(&mut self, s: &str, pt: f32, bold: bool, max: i32) -> String {
        let px = self.font_px(pt);
        self.fonts.fit(s, px, bold, max)
    }
    /// Draw `s` with its line box's top-left at (x, top). Returns the advance in device pixels.
    pub fn text(&mut self, x: i32, top: i32, s: &str, pt: f32, color: crate::Rgb, bold: bool) -> i32 {
        self.text_a(x, top, s, pt, color, bold, 1.0)
    }
    pub fn text_a(&mut self, x: i32, top: i32, s: &str, pt: f32, color: crate::Rgb, bold: bool, alpha: f32) -> i32 {
        let px = self.font_px(pt);
        let (asc, _) = self.fonts.metrics(px);
        let mut pen = x as f32;
        for c in s.chars() {
            let g = self.fonts.glyph(c, px, bold);
            self.cv.glyph(g, pen.round() as i32, top + asc, color, alpha);
            pen += g.adv;
        }
        (pen - x as f32).round() as i32
    }
}

pub trait App {
    fn kind(&self) -> SurfaceKind;
    /// Layer surfaces: the output to appear on at start-up.
    fn pick_output(&mut self, _outs: &[OutputDesc]) -> Option<String> {
        None
    }
    fn draw(&mut self, f: &mut Frame);
    fn key(&mut self, _k: &Key, _ctx: &mut Ctx) {}
    /// Left click at (x, y) in device pixels of the drawn frame (same units as `Frame` drawing).
    fn click(&mut self, _x: i32, _y: i32, _ctx: &mut Ctx) {}
    /// Every pointer event (device px of the surface `surface`, its index as in `Frame::index`).
    /// Left press also calls `click`.
    fn pointer(&mut self, _ev: Ptr, _x: i32, _y: i32, _surface: usize, _ctx: &mut Ctx) {}
    /// Wheel: positive = down, in lines.
    fn scroll(&mut self, _lines: i32, _ctx: &mut Ctx) {}
    /// Called every `tick_ms` milliseconds. Read at start (0 = no timer); returning 0 later stops it.
    fn tick(&mut self, _ctx: &mut Ctx) {}
    fn tick_ms(&self) -> u64 {
        0
    }
    /// Lines from the channel passed to [`run`].
    fn message(&mut self, _m: String, _ctx: &mut Ctx) {}
    fn outputs_changed(&mut self, _outs: &[OutputDesc], _ctx: &mut Ctx) {}
    /// Lock apps: the compositor confirmed the lock (true) or refused it (false).
    fn lock_state(&mut self, _locked: bool, _ctx: &mut Ctx) {}
    /// Keyboard focus arrived (true) or left (false).
    fn focus(&mut self, _focused: bool, _ctx: &mut Ctx) {}
}

// ====================================================================== internals
enum Role {
    Window(Window),
    Layer(LayerSurface),
    Lock(SessionLockSurface),
}

struct Surf {
    id: u32,
    role: Role,
    output: Option<wl_output::WlOutput>,
    size: (u32, u32),
    scale: f64,
    frac: Option<WpFractionalScaleV1>,
    vp: Option<WpViewport>,
    configured: bool,
    frame_pending: bool,
    dirty: bool,
}

impl Surf {
    fn wl(&self) -> &wl_surface::WlSurface {
        match &self.role {
            Role::Window(w) => w.wl_surface(),
            Role::Layer(l) => l.wl_surface(),
            Role::Lock(l) => l.wl_surface(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct SurfTag(u32);

struct Shell {
    app: Box<dyn App>,
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: CompositorState,
    xdg: Option<XdgShell>,
    layer: Option<LayerShell>,
    lock_state: Option<SessionLockState>,
    lock: Option<SessionLock>,
    shm: Shm,
    pool: SlotPool,
    frac_mgr: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    cursor: Option<CursorShapeManager>,
    cursor_dev: Option<smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1>,
    wheel: f64,
    mods: Modifiers,
    surfs: Vec<Surf>,
    next_id: u32,
    fonts: Fonts,
    theme: Theme,
    focused: Option<u32>,
    exit: bool,
    unlocking: bool,
    loop_handle: smithay_client_toolkit::reexports::calloop::LoopHandle<'static, Shell>,
    outs_cache: Vec<OutputDesc>,
    qh: QueueHandle<Shell>,
    hidden: bool,
    tick_token: Option<smithay_client_toolkit::reexports::calloop::RegistrationToken>,
}

/// Run `app` until it quits. `messages`: optional channel whose lines reach `App::message`.
pub fn run(app: Box<dyn App>, messages: Option<Channel<String>>) -> Result<(), String> {
    // headless preview: SHADOW_SNAPSHOT=out.ppm [SHADOW_SNAPSHOT_SCALE=1.5] renders one frame of
    // the app at its initial size and exits (screenshots and visual checks; no compositor)
    if let Ok(path) = std::env::var("SHADOW_SNAPSHOT") {
        return snapshot(app, messages, &path);
    }
    let conn = Connection::connect_to_env().map_err(|e| format!("cannot connect to the Wayland compositor: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<Shell>(&conn).map_err(|e| format!("wayland registry: {e}"))?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<'static, Shell> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let theme = Theme::load();
    let fonts = Fonts::load(&theme.font)?;
    let shm = Shm::bind(&globals, &qh).map_err(|_| "wl_shm missing")?;
    let pool = SlotPool::new(512 * 512 * 4, &shm).map_err(|e| e.to_string())?;
    let kind = app.kind();
    let mut sh = Shell {
        app,
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor: CompositorState::bind(&globals, &qh).map_err(|_| "wl_compositor missing")?,
        xdg: XdgShell::bind(&globals, &qh).ok(),
        layer: LayerShell::bind(&globals, &qh).ok(),
        lock_state: matches!(kind, SurfaceKind::Lock).then(|| SessionLockState::new(&globals, &qh)),
        lock: None,
        shm,
        pool,
        frac_mgr: globals.bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ()).ok(),
        viewporter: globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ()).ok(),
        keyboard: None,
        pointer: None,
        cursor: CursorShapeManager::bind(&globals, &qh).ok(),
        cursor_dev: None,
        wheel: 0.0,
        mods: Modifiers::default(),
        surfs: vec![],
        next_id: 1,
        fonts,
        theme,
        focused: None,
        exit: false,
        unlocking: false,
        loop_handle: event_loop.handle(),
        outs_cache: vec![],
        qh: qh.clone(),
        hidden: false,
        tick_token: None,
    };
    // learn the outputs before placing anything
    queue.roundtrip(&mut sh).map_err(|e| e.to_string())?;
    queue.roundtrip(&mut sh).map_err(|e| e.to_string())?;
    sh.outs_cache = sh.outputs();
    match kind {
        SurfaceKind::Window { app_id, title, size } => {
            let xdg = sh.xdg.as_ref().ok_or("xdg_wm_base missing")?;
            let surface = sh.compositor.create_surface(&qh);
            let window = xdg.create_window(surface, WindowDecorations::RequestServer, &qh);
            window.set_title(title);
            window.set_app_id(app_id);
            window.set_min_size(Some((200, 120)));
            sh.add_surf(Role::Window(window), None, size, &qh);
            sh.surfs.last().unwrap().wl().commit();
        }
        SurfaceKind::Layer { .. } => {
            let outs = sh.outs_cache.clone();
            let want = sh.app.pick_output(&outs);
            sh.place_layer(want, &qh)?;
        }
        SurfaceKind::Background { .. } => sh.cover_background(&qh)?,
        SurfaceKind::Lock => {
            let st = sh.lock_state.as_ref().ok_or("lock state")?;
            sh.lock = Some(st.lock(&qh).map_err(|_| "ext-session-lock-v1 is not available (another locker running?)")?);
        }
    }
    WaylandSource::new(conn.clone(), queue).insert(event_loop.handle()).map_err(|e| e.to_string())?;
    if let Some(ch) = messages {
        event_loop
            .handle()
            .insert_source(ch, |ev, _, sh: &mut Shell| {
                if let channel::Event::Msg(m) = ev {
                    let mut ctx = Ctx::default();
                    sh.app.message(m, &mut ctx);
                    sh.apply(ctx);
                }
            })
            .map_err(|e| e.to_string())?;
    }
    sh.arm_tick();
    while !sh.exit {
        event_loop.dispatch(None, &mut sh).map_err(|e| e.to_string())?;
        sh.render_all(&qh);
        let _ = conn.flush();
    }
    if let Some(l) = sh.lock.take() {
        if sh.unlocking {
            l.unlock();
            let _ = conn.roundtrip();
        }
    }
    Ok(())
}

impl Shell {
    fn outputs(&self) -> Vec<OutputDesc> {
        self.output_state
            .outputs()
            .filter_map(|o| self.output_state.info(&o))
            .map(|i| {
                let cur = i.modes.iter().find(|m| m.current).or(i.modes.first());
                OutputDesc {
                    name: i.name.clone().unwrap_or_else(|| format!("output-{}", i.id)),
                    make: i.make.clone(),
                    model: i.model.clone(),
                    px: cur.map(|m| m.dimensions).unwrap_or((0, 0)),
                    mm: i.physical_size,
                    refresh_mhz: cur.map(|m| m.refresh_rate).unwrap_or(0),
                }
            })
            .collect()
    }

    fn output_named(&self, name: &str) -> Option<wl_output::WlOutput> {
        self.output_state.outputs().find(|o| self.output_state.info(o).and_then(|i| i.name).as_deref() == Some(name))
    }

    fn output_name(&self, o: &wl_output::WlOutput) -> Option<String> {
        self.output_state.info(o).and_then(|i| i.name)
    }

    fn add_surf(&mut self, role: Role, output: Option<wl_output::WlOutput>, size: (u32, u32), qh: &QueueHandle<Shell>) {
        let id = self.next_id;
        self.next_id += 1;
        let mut s = Surf { id, role, output, size, scale: 1.0, frac: None, vp: None, configured: false, frame_pending: false, dirty: true };
        if let (Some(fm), Some(vpr)) = (&self.frac_mgr, &self.viewporter) {
            s.frac = Some(fm.get_fractional_scale(s.wl(), qh, SurfTag(id)));
            s.vp = Some(vpr.get_viewport(s.wl(), qh, ()));
        }
        // until the compositor says otherwise, assume the ideal scale of the output we're on
        if let Some(o) = &s.output {
            if let Some(i) = self.output_state.info(o) {
                s.scale = i.scale_factor.max(1) as f64;
            }
        }
        self.surfs.push(s);
    }

    fn place_layer(&mut self, output: Option<String>, qh: &QueueHandle<Shell>) -> Result<(), String> {
        let SurfaceKind::Layer { namespace, layer, anchor, size, margin, exclusive, keyboard } = self.app.kind() else { return Ok(()) };
        self.surfs.retain(|s| !matches!(s.role, Role::Layer(_)));
        let ls = self.layer.as_ref().ok_or("zwlr_layer_shell_v1 missing (niri provides it)")?;
        let out = output.as_deref().and_then(|n| self.output_named(n));
        let surface = self.compositor.create_surface(qh);
        let l = ls.create_layer_surface(qh, surface, layer, Some(namespace), out.as_ref());
        l.set_anchor(anchor);
        l.set_size(size.0, size.1);
        l.set_margin(margin.0, margin.1, margin.2, margin.3);
        l.set_exclusive_zone(exclusive);
        l.set_keyboard_interactivity(match keyboard {
            LayerKeyboard::None => KeyboardInteractivity::None,
            LayerKeyboard::OnDemand => KeyboardInteractivity::OnDemand,
            LayerKeyboard::Exclusive => KeyboardInteractivity::Exclusive,
        });
        l.commit();
        self.add_surf(Role::Layer(l), out, size, qh);
        Ok(())
    }

    /// (Re)start the tick timer from `App::tick_ms` (0 = none).
    fn arm_tick(&mut self) {
        if let Some(t) = self.tick_token.take() {
            self.loop_handle.remove(t);
        }
        let tick = self.app.tick_ms();
        if tick == 0 {
            return;
        }
        self.tick_token = self
            .loop_handle
            .insert_source(Timer::from_duration(Duration::from_millis(tick)), |_, _, sh: &mut Shell| {
                let mut ctx = Ctx::default();
                ctx.wake = false;
                sh.app.tick(&mut ctx);
                // a wake asked for inside tick is the same timer: just take the new period
                ctx.wake = false;
                sh.apply(ctx);
                // 0 = the app no longer wants ticks: stop (never spin at the 16 ms floor)
                match sh.app.tick_ms() {
                    0 => {
                        sh.tick_token = None;
                        TimeoutAction::Drop
                    }
                    t => TimeoutAction::ToDuration(Duration::from_millis(t.max(16))),
                }
            })
            .ok();
    }

    fn apply(&mut self, ctx: Ctx) {
        if ctx.wake {
            self.arm_tick();
        }
        if ctx.quit {
            self.exit = true;
        }
        if ctx.unlock {
            self.unlocking = true;
            self.exit = true;
        }
        if let Some(t) = ctx.title {
            for s in &self.surfs {
                if let Role::Window(w) = &s.role {
                    w.set_title(t.clone());
                }
            }
        }
        let qh = self.qh();
        if let Some(v) = ctx.visible {
            self.hidden = !v;
            let shown = self.surfs.iter().any(|s| matches!(s.role, Role::Layer(_)));
            if !v && shown {
                self.surfs.retain(|s| !matches!(s.role, Role::Layer(_)));
            } else if v && !shown {
                let outs = self.outputs();
                let want = self.app.pick_output(&outs);
                let _ = self.place_layer(want, &qh);
            }
        }
        if let Some(to) = ctx.move_to.filter(|_| !self.hidden) {
            let cur = self.surfs.iter().find(|s| matches!(s.role, Role::Layer(_))).and_then(|s| s.output.as_ref()).and_then(|o| self.output_name(o));
            if cur != to || self.surfs.is_empty() {
                let _ = self.place_layer(to, &qh);
            }
        }
        if let Some((w, h)) = ctx.resize {
            for s in &mut self.surfs {
                if let Role::Layer(l) = &s.role {
                    if s.size != (w, h) {
                        l.set_size(w, h);
                        l.commit();
                    }
                }
            }
        }
        if ctx.redraw {
            for s in &mut self.surfs {
                s.dirty = true;
            }
        }
    }

    fn qh(&self) -> QueueHandle<Shell> {
        self.qh.clone()
    }

    fn render_all(&mut self, qh: &QueueHandle<Shell>) {
        let outs = self.outs_cache.clone();
        for i in 0..self.surfs.len() {
            let s = &self.surfs[i];
            if !s.configured || s.frame_pending || !s.dirty || s.size.0 == 0 || s.size.1 == 0 {
                continue;
            }
            let scale = s.scale;
            let (lw, lh) = s.size;
            let (w, h) = (((lw as f64) * scale).round() as i32, ((lh as f64) * scale).round() as i32);
            let out_name = s.output.as_ref().and_then(|o| self.output_name(o));
            let ideal = out_name.as_ref().and_then(|n| outs.iter().find(|d| &d.name == n)).and_then(|d| d.ideal_scale());
            let corr = scale::text_correction(scale, ideal);
            let Ok((buffer, canvas)) = self.pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) else { continue };
            let focused = self.focused == Some(s.id);
            let want;
            {
                let mut f = Frame {
                    cv: Canvas { buf: canvas, w: w as usize, h: h as usize },
                    scale: scale as f32,
                    text: (scale * corr) as f32,
                    fonts: &mut self.fonts,
                    theme: &self.theme,
                    output: out_name.as_deref(),
                    index: i,
                    focused,
                    want_size: None,
                    logical: (lw, lh),
                };
                self.app.draw(&mut f);
                want = f.want_size;
            }
            let s = &mut self.surfs[i];
            let wl = s.wl().clone();
            match &s.vp {
                Some(vp) => vp.set_destination(lw as i32, lh as i32),
                None => wl.set_buffer_scale(scale.round().max(1.0) as i32),
            }
            wl.damage_buffer(0, 0, w, h);
            wl.frame(qh, FrameCallbackData(wl.clone()));
            let _ = buffer.attach_to(&wl);
            wl.commit();
            s.frame_pending = true;
            s.dirty = false;
            if let (Some((nw, nh)), Role::Layer(l)) = (want, &s.role) {
                if (nw, nh) != s.size && nw > 0 && nh > 0 {
                    l.set_size(nw, nh);
                    l.commit();
                }
            }
        }
    }

    fn surf_mut(&mut self, wl: &wl_surface::WlSurface) -> Option<&mut Surf> {
        self.surfs.iter_mut().find(|s| s.wl() == wl)
    }

    fn outputs_updated(&mut self, qh: &QueueHandle<Shell>) {
        let outs = self.outputs();
        if outs == self.outs_cache {
            return;
        }
        self.outs_cache = outs.clone();
        // lock: every monitor must be covered
        if let Some(lock) = self.lock.clone() {
            if lock.is_locked() {
                self.cover_outputs(&lock, qh);
            }
        }
        self.surfs.retain(|s| s.output.as_ref().map_or(true, |o| o.is_alive()));
        if matches!(self.app.kind(), SurfaceKind::Background { .. }) {
            let _ = self.cover_background(qh);
        }
        let mut ctx = Ctx::default();
        self.app.outputs_changed(&outs, &mut ctx);
        if self.surfs.is_empty() && !self.hidden && ctx.move_to.is_none() && matches!(self.app.kind(), SurfaceKind::Layer { .. }) {
            ctx.move_to = Some(self.app.pick_output(&outs));
        }
        ctx.redraw = true;
        self.apply(ctx);
    }

    fn cover_background(&mut self, qh: &QueueHandle<Shell>) -> Result<(), String> {
        let SurfaceKind::Background { namespace, keyboard } = self.app.kind() else { return Ok(()) };
        let outs: Vec<wl_output::WlOutput> = self.output_state.outputs().filter(|o| !self.surfs.iter().any(|s| s.output.as_ref() == Some(o))).collect();
        let mut made = vec![];
        {
            let ls = self.layer.as_ref().ok_or("zwlr_layer_shell_v1 missing (niri provides it)")?;
            for o in outs {
                let surface = self.compositor.create_surface(qh);
                let l = ls.create_layer_surface(qh, surface, Layer::Background, Some(namespace.clone()), Some(&o));
                l.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
                l.set_size(0, 0);
                l.set_exclusive_zone(-1);
                l.set_keyboard_interactivity(match keyboard {
                    LayerKeyboard::None => KeyboardInteractivity::None,
                    LayerKeyboard::OnDemand => KeyboardInteractivity::OnDemand,
                    LayerKeyboard::Exclusive => KeyboardInteractivity::Exclusive,
                });
                l.commit();
                made.push((l, o));
            }
        }
        for (l, o) in made {
            self.add_surf(Role::Layer(l), Some(o), (1, 1), qh);
        }
        Ok(())
    }

    fn cover_outputs(&mut self, lock: &SessionLock, qh: &QueueHandle<Shell>) {
        let outs: Vec<wl_output::WlOutput> = self.output_state.outputs().collect();
        for o in outs {
            if self.surfs.iter().any(|s| s.output.as_ref() == Some(&o)) {
                continue;
            }
            let surface = self.compositor.create_surface(qh);
            let ls = lock.create_lock_surface(surface, &o, qh);
            self.add_surf(Role::Lock(ls), Some(o), (0, 0), qh);
        }
    }

    fn on_key(&mut self, ev: KeyEvent) {
        let k = Key { sym: ev.keysym, text: ev.utf8, ctrl: self.mods.ctrl, alt: self.mods.alt, shift: self.mods.shift, logo: self.mods.logo, caps: self.mods.caps_lock };
        let mut ctx = Ctx::default();
        self.app.key(&k, &mut ctx);
        self.apply(ctx);
    }
}

// ====================================================================== handlers
impl CompositorHandler for Shell {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, factor: i32) {
        if let Some(s) = self.surf_mut(surface) {
            if s.frac.is_none() && factor > 0 && (factor as f64 - s.scale).abs() > 1e-9 {
                s.scale = factor as f64;
                s.dirty = true;
            }
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, _: u32) {
        if let Some(s) = self.surf_mut(surface) {
            s.frame_pending = false;
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, output: &wl_output::WlOutput) {
        if let Some(s) = self.surf_mut(surface) {
            if matches!(s.role, Role::Window(_)) && s.output.as_ref() != Some(output) {
                s.output = Some(output.clone());
                s.dirty = true;
            }
        }
    }
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Shell {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: wl_output::WlOutput) {
        if !self.surfs.is_empty() || self.lock.is_some() {
            self.outputs_updated(qh);
        }
    }
    fn update_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: wl_output::WlOutput) {
        if !self.surfs.is_empty() || self.lock.is_some() {
            self.outputs_updated(qh);
        }
    }
    fn output_destroyed(&mut self, _: &Connection, qh: &QueueHandle<Self>, o: wl_output::WlOutput) {
        self.surfs.retain(|s| s.output.as_ref() != Some(&o) || matches!(s.role, Role::Window(_)));
        self.outputs_updated(qh);
    }
}

impl WindowHandler for Shell {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window, c: WindowConfigure, _: u32) {
        if let Some(s) = self.surfs.iter_mut().find(|s| matches!(&s.role, Role::Window(w) if w == window)) {
            if let (Some(w), Some(h)) = c.new_size {
                s.size = (w.get(), h.get());
            }
            s.configured = true;
            s.dirty = true;
        }
    }
}

impl LayerShellHandler for Shell {
    fn closed(&mut self, _: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface) {
        self.surfs.retain(|s| !matches!(&s.role, Role::Layer(l) if l == layer));
        if matches!(self.app.kind(), SurfaceKind::Background { .. }) {
            let _ = self.cover_background(qh);
            return;
        }
        let outs = self.outputs();
        let want = self.app.pick_output(&outs);
        let _ = self.place_layer(want, qh);
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        if let Some(s) = self.surfs.iter_mut().find(|s| matches!(&s.role, Role::Layer(l) if l == layer)) {
            let (w, h) = c.new_size;
            s.size = (if w > 0 { w } else { s.size.0.max(1) }, if h > 0 { h } else { s.size.1.max(1) });
            s.configured = true;
            s.dirty = true;
        }
    }
}

impl SessionLockHandler for Shell {
    fn locked(&mut self, _: &Connection, qh: &QueueHandle<Self>, lock: SessionLock) {
        self.cover_outputs(&lock, qh);
        let mut ctx = Ctx::default();
        self.app.lock_state(true, &mut ctx);
        self.apply(ctx);
    }
    fn finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: SessionLock) {
        let mut ctx = Ctx::default();
        self.app.lock_state(false, &mut ctx);
        self.apply(ctx);
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: SessionLockSurface, c: SessionLockSurfaceConfigure, _: u32) {
        if let Some(s) = self.surf_mut(surface.wl_surface()) {
            s.size = c.new_size;
            s.configured = true;
            s.dirty = true;
        }
    }
}

impl ShmHandler for Shell {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl SeatHandler for Shell {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            let kb = self.seat_state.get_keyboard_with_repeat(qh, &seat, None, self.loop_handle.clone(), Box::new(|sh: &mut Shell, _, ev| sh.on_key(ev)));
            if let Ok(k) = kb {
                self.keyboard = Some(k);
            }
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            if let Ok(p) = self.seat_state.get_pointer(qh, &seat) {
                self.cursor_dev = self.cursor.as_ref().map(|m| m.get_shape_device(&p, qh));
                self.pointer = Some(p);
            }
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if cap == Capability::Pointer {
            if let Some(d) = self.cursor_dev.take() {
                d.destroy();
            }
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Shell {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, surface: &wl_surface::WlSurface, _: u32, _: &[u32], _: &[Keysym]) {
        self.focused = self.surfs.iter().find(|s| s.wl() == surface).map(|s| s.id);
        for s in &mut self.surfs {
            s.dirty = true;
        }
        let mut ctx = Ctx::default();
        self.app.focus(true, &mut ctx);
        self.apply(ctx);
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        self.focused = None;
        for s in &mut self.surfs {
            s.dirty = true;
        }
        let mut ctx = Ctx::default();
        self.app.focus(false, &mut ctx);
        self.apply(ctx);
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.on_key(event);
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.on_key(event);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: RawModifiers, _: u32) {
        self.mods = m;
    }
}

impl PointerHandler for Shell {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
        for ev in events {
            let idx = self.surfs.iter().position(|s| s.wl() == &ev.surface).unwrap_or(0);
            let scale = self.surfs.get(idx).map_or(1.0, |s| s.scale);
            let (x, y) = ((ev.position.0 * scale).round() as i32, (ev.position.1 * scale).round() as i32);
            let mut ctx = Ctx::default();
            match ev.kind {
                PointerEventKind::Motion { .. } => self.app.pointer(Ptr::Move, x, y, idx, &mut ctx),
                PointerEventKind::Leave { .. } => self.app.pointer(Ptr::Leave, x, y, idx, &mut ctx),
                PointerEventKind::Release { button: BTN_LEFT, .. } => self.app.pointer(Ptr::Up, x, y, idx, &mut ctx),
                // without this the compositor shows no cursor (or a stale one) over our windows
                PointerEventKind::Enter { serial } => {
                    if let Some(d) = &self.cursor_dev {
                        d.set_shape(serial, Shape::Default);
                    }
                    self.app.pointer(Ptr::Move, x, y, idx, &mut ctx);
                }
                PointerEventKind::Press { button: BTN_LEFT, .. } => {
                    self.app.pointer(Ptr::Down, x, y, idx, &mut ctx);
                    self.app.click(x, y, &mut ctx);
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let d = if vertical.discrete != 0 { vertical.discrete as f64 } else { vertical.absolute / 15.0 };
                    self.wheel += d;
                    let n = self.wheel.trunc() as i32;
                    if n != 0 {
                        self.wheel -= n as f64;
                        self.app.scroll(n, &mut ctx);
                    }
                }
                _ => continue,
            }
            self.apply(ctx);
        }
    }
}

delegate_registry!(Shell);

impl ProvidesRegistryState for Shell {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(Shell);

impl Dispatch<WpFractionalScaleManagerV1, ()> for Shell {
    fn event(_: &mut Self, _: &WpFractionalScaleManagerV1, _: <WpFractionalScaleManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpViewporter, ()> for Shell {
    fn event(_: &mut Self, _: &WpViewporter, _: <WpViewporter as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpViewport, ()> for Shell {
    fn event(_: &mut Self, _: &WpViewport, _: <WpViewport as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpFractionalScaleV1, SurfTag> for Shell {
    fn event(sh: &mut Self, _: &WpFractionalScaleV1, ev: wp_fractional_scale_v1::Event, tag: &SurfTag, _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = ev {
            let v = (scale as f64 / 120.0).clamp(0.5, 4.0);
            if let Some(s) = sh.surfs.iter_mut().find(|s| s.id == tag.0) {
                if (s.scale - v).abs() > 1e-9 {
                    s.scale = v;
                    s.dirty = true;
                }
            }
        }
    }
}

/// Render one frame without a compositor (see `run`). Writes a binary PPM.
fn snapshot(mut app: Box<dyn App>, messages: Option<Channel<String>>, path: &str) -> Result<(), String> {
    let theme = Theme::load();
    let mut fonts = Fonts::load(&theme.font)?;
    let scale: f64 = std::env::var("SHADOW_SNAPSHOT_SCALE").ok().and_then(|v| v.parse().ok()).unwrap_or(1.5);
    let (lw, lh) = match app.kind() {
        SurfaceKind::Window { size, .. } => size,
        SurfaceKind::Layer { size, .. } => (if size.0 == 0 { 1280 } else { size.0 }, if size.1 == 0 { if size.0 == 0 { 800 } else { 600 } } else { size.1 }),
        SurfaceKind::Lock | SurfaceKind::Background { .. } => (1280, 800),
    };
    let lh = std::env::var("SHADOW_SNAPSHOT_H").ok().and_then(|v| v.parse().ok()).unwrap_or(lh);
    if matches!(app.kind(), SurfaceKind::Layer { .. }) {
        let _ = app.pick_output(&[]);
    }
    // let the app take in messages it would get at start-up (worker threads) for a moment
    if let Some(ch) = messages {
        let wait: u64 = std::env::var("SHADOW_SNAPSHOT_WAIT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        if wait > 0 {
            let mut el: EventLoop<'static, (Box<dyn App>, Ctx)> = EventLoop::try_new().map_err(|e| e.to_string())?;
            el.handle()
                .insert_source(ch, |ev, _, st| {
                    if let channel::Event::Msg(m) = ev {
                        st.0.message(m, &mut st.1)
                    }
                })
                .map_err(|e| e.to_string())?;
            let mut st = (app, Ctx::default());
            let end = std::time::Instant::now() + Duration::from_millis(wait);
            while std::time::Instant::now() < end {
                let _ = el.dispatch(Some(Duration::from_millis(20)), &mut st);
            }
            app = st.0;
        }
    }
    // SHADOW_SNAPSHOT_POINTER=X,Y (logical px): the pointer rests there (reactive backgrounds)
    if let Some((x, y)) = std::env::var("SHADOW_SNAPSHOT_POINTER").ok().and_then(|v| v.split_once(',').and_then(|(x, y)| Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?)))) {
        let (x, y) = ((x * scale) as i32, (y * scale) as i32);
        app.pointer(Ptr::Move, x - 40, y - 30, 0, &mut Ctx::default());
        app.pointer(Ptr::Move, x, y, 0, &mut Ctx::default());
    }
    let ticks: u32 = std::env::var("SHADOW_SNAPSHOT_TICKS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    for _ in 0..ticks {
        std::thread::sleep(Duration::from_millis(app.tick_ms().clamp(1, 1000)));
        app.tick(&mut Ctx::default());
    }
    let mut size = (lw, lh);
    for _pass in 0..4 {
        let (w, h) = ((size.0 as f64 * scale).round() as usize, (size.1 as f64 * scale).round() as usize);
        let mut buf = vec![0u8; w * h * 4];
        let want;
        {
            let mut f = Frame { cv: Canvas { buf: &mut buf, w, h }, scale: scale as f32, text: scale as f32, fonts: &mut fonts, theme: &theme, output: None, index: 0, focused: true, want_size: None, logical: size };
            app.draw(&mut f);
            want = f.want_size;
        }
        // SHADOW_SNAPSHOT_BENCH=N: draw N more frames and report the average (performance checks)
        if let Some(n) = std::env::var("SHADOW_SNAPSHOT_BENCH").ok().and_then(|v| v.parse::<u32>().ok()).filter(|n| *n > 0) {
            let t = std::time::Instant::now();
            for i in 0..n {
                let mut f = Frame { cv: Canvas { buf: &mut buf, w, h }, scale: scale as f32, text: scale as f32, fonts: &mut fonts, theme: &theme, output: None, index: 0, focused: true, want_size: None, logical: size };
                app.pointer(Ptr::Move, (w as i32 / 3) + i as i32 * 3, (h as i32 / 2) + i as i32, 0, &mut Ctx::default());
                app.tick(&mut Ctx::default());
                app.draw(&mut f);
            }
            eprintln!("{w}x{h}: {:.2} ms / frame over {n} frames", t.elapsed().as_secs_f64() * 1000.0 / n as f64);
        }
        if let Some(ws) = want.filter(|ws| *ws != size) {
            size = ws;
            continue;
        }
        // composite onto a dark desktop so translucent panels look as they would
        let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
        for p in buf.chunks_exact(4) {
            let a = p[3] as f32 / 255.0;
            let bgd = [0x20u8, 0x22, 0x28];
            for (k, ch) in [2usize, 1, 0].iter().enumerate() {
                out.push((p[*ch] as f32 + bgd[k] as f32 * (1.0 - a)).round().min(255.0) as u8);
            }
        }
        return std::fs::write(path, out).map_err(|e| e.to_string());
    }
    Err("the app kept changing its size".into())
}
