//! Compositor state and Wayland protocol handlers.
use crate::config::{self, Config};
use crate::orbit::{self, Geo, Ring, Size, R};
use crate::ui::Ui;
use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{gles::GlesRenderer, utils::on_commit_buffer_handler, ImportDma},
    },
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_fractional_scale, delegate_layer_shell, delegate_output,
    delegate_primary_selection, delegate_seat, delegate_shm, delegate_viewporter, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output, LayerSurface, PopupKind, PopupManager, Space, Window, WindowSurfaceType},
    input::{keyboard::XkbConfig, Seat, SeatHandler, SeatState},
    output::Output,
    reexports::{
        calloop::{LoopHandle, LoopSignal},
        wayland_protocols::xdg::{decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecoMode, shell::server::xdg_toplevel},
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_output, wl_seat, wl_surface::WlSurface},
            Client, DisplayHandle, Resource,
        },
    },
    utils::{Logical, Point, Rectangle, Serial},
    wayland::{
        buffer::BufferHandler,
        compositor::{get_parent, is_sync_subsurface, send_surface_state, with_states, CompositorClientState, CompositorHandler, CompositorState},
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        fractional_scale::{with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState},
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler},
            primary_selection::{set_primary_focus, PrimarySelectionHandler, PrimarySelectionState},
            SelectionHandler,
        },
        shell::{
            wlr_layer::{Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData, WlrLayerShellHandler, WlrLayerShellState},
            xdg::{decoration::{XdgDecorationHandler, XdgDecorationState}, PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState, XdgToplevelSurfaceData},
        },
        shm::{ShmHandler, ShmState},
        viewporter::ViewporterState,
    },
};
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub struct Win {
    pub id: u64,
    pub window: Window,
    pub size: Size,
    pub app_id: String,
    pub title: String,
    pub pid: Option<i32>,
    pub pointer: bool,
    /// CPU % samples for the ring sparkline (low frequency)
    pub spark: Vec<f32>,
    pub cpu_prev: Option<(u64, u64)>,
    pub rss_mb: u64,
}

pub enum Anim {
    /// visual rects of every window when the transition started
    Move { start: Instant, dur: Duration, from: HashMap<u64, R> },
}

pub enum Backend {
    None,
    Winit(crate::winit::WinitData),
    Udev(crate::udev::UdevData),
}

pub struct State {
    pub dh: DisplayHandle,
    pub handle: LoopHandle<'static, State>,
    pub signal: LoopSignal,
    pub start: Instant,
    pub socket_name: String,
    pub cfg: Config,
    pub cfg_problem: Option<String>,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub decoration_state: XdgDecorationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<State>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub fractional_state: FractionalScaleManagerState,
    pub viewporter_state: ViewporterState,
    pub popups: PopupManager,
    pub seat: Seat<State>,

    pub space: Space<Window>,
    pub output: Option<Output>,
    pub backend: Backend,

    pub wins: Vec<Win>,
    pub ring: Ring,
    pub next_id: u64,
    pub anim: Option<Anim>,
    pub hud_visible: bool,
    pub hud: Option<crate::hud::Snapshot>,
    pub hud_extra: Vec<String>,
    pub refresh_hz: u32,
    pub wanted_refresh: Option<u32>,

    pub ui: Option<Ui>,
    pub greeter_mode: bool,
    pub boot: Option<Instant>,

    pub pointer_loc: Point<f64, Logical>,
    pub pointer_live: bool,
    pub suppressed_keys: Vec<u32>,
    pub key_times: Vec<Instant>,
    pub last_input: Instant,

    pub text: crate::text::Text,
    pub render: crate::render::RenderCache,
    pub lua: Option<crate::luahost::LuaHost>,
    pub dirty: bool,
    pub mem_high_seen: u64,
    pub binds: Vec<crate::input::Bind>,
    pub ui_tx: Option<smithay::reexports::calloop::channel::Sender<crate::ui::UiMsg>>,
    pub exit_code: i32,
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

impl State {
    pub fn new(dh: DisplayHandle, handle: LoopHandle<'static, State>, signal: LoopSignal, socket_name: String, greeter: bool) -> State {
        let (cfg, problem) = config::load(None);
        let compositor_state = CompositorState::new::<State>(&dh);
        let xdg_shell_state = XdgShellState::new::<State>(&dh);
        let decoration_state = XdgDecorationState::new::<State>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<State>(&dh);
        let shm_state = ShmState::new::<State>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<State>(&dh);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<State>(&dh);
        let primary_selection_state = PrimarySelectionState::new::<State>(&dh);
        let fractional_state = FractionalScaleManagerState::new::<State>(&dh);
        let viewporter_state = ViewporterState::new::<State>(&dh);
        let mut seat: Seat<State> = seat_state.new_wl_seat(&dh, "seat0");
        let xkb = XkbConfig { layout: &cfg.input.xkb_layout, variant: &cfg.input.xkb_variant, options: (!cfg.input.xkb_options.is_empty()).then(|| cfg.input.xkb_options.clone()), ..Default::default() };
        if seat.add_keyboard(xkb, cfg.input.repeat_delay, cfg.input.repeat_rate).is_err() {
            seat.add_keyboard(XkbConfig::default(), 300, 40).expect("keyboard");
        }
        seat.add_pointer();
        let text = crate::text::Text::new(&cfg.look.font);
        State {
            dh, handle, signal, start: Instant::now(), socket_name, cfg, cfg_problem: problem,
            compositor_state, xdg_shell_state, decoration_state, layer_shell_state, shm_state,
            dmabuf_state: DmabufState::new(), dmabuf_global: None,
            output_manager_state, seat_state, data_device_state, primary_selection_state, fractional_state, viewporter_state,
            popups: PopupManager::default(), seat,
            space: Space::default(), output: None, backend: Backend::None,
            wins: vec![], ring: Ring::default(), next_id: 1, anim: None, hud_visible: true, hud: None, hud_extra: vec![],
            refresh_hz: 60, wanted_refresh: None,
            ui: None, greeter_mode: greeter, boot: (!greeter).then(Instant::now),
            pointer_loc: (0.0, 0.0).into(), pointer_live: false, suppressed_keys: vec![], key_times: vec![], last_input: Instant::now(),
            text, render: Default::default(), lua: None, dirty: true, mem_high_seen: 0,
            binds: vec![], ui_tx: None, exit_code: 0,
        }
    }

    // ------------------------------------------------------------------ geometry
    pub fn output_size(&self) -> (f64, f64) {
        self.output.as_ref().and_then(|o| self.space.output_geometry(o)).map(|g| (g.size.w as f64, g.size.h as f64)).unwrap_or((1280.0, 800.0))
    }
    pub fn scale(&self) -> f64 {
        self.output.as_ref().map(|o| o.current_scale().fractional_scale()).unwrap_or(1.0)
    }
    /// HUD bar height in logical px
    pub fn bar_h(&self) -> f64 {
        if self.cfg.hud.enabled && self.hud_visible && !self.greeter_mode { self.cfg.hud.height_px as f64 / self.scale() } else { 0.0 }
    }
    pub fn geo(&self) -> Geo {
        let (w, h) = self.output_size();
        let b = self.bar_h();
        let l = &self.cfg.layout;
        Geo { area: R { x: 0.0, y: b, w, h: h - 2.0 * b }, rx: l.ring_rx, rz: l.ring_rz, y_off: l.ring_y, node_w: l.node_w as f64, node_h: l.node_h as f64 }
    }
    pub fn win(&self, id: u64) -> Option<&Win> {
        self.wins.iter().find(|w| w.id == id)
    }
    pub fn center_rect(&self, id: u64) -> R {
        let s = self.win(id).map(|w| w.size).unwrap_or(Size { preset: self.cfg.layout.default_preset, full: false });
        let pct = self.cfg.layout.presets.get(s.preset).copied().unwrap_or(65);
        self.geo().center(pct, self.cfg.layout.height, s.full)
    }
    /// where every window is drawn right now, without animation: (id, rect, depth scale, is_center)
    pub fn targets(&self) -> Vec<(u64, R, f64, bool)> {
        let g = self.geo();
        let n = self.ring.order.len().saturating_sub(1);
        self.ring.order.iter().enumerate().map(|(i, id)| {
            if i == 0 {
                (*id, self.center_rect(*id), 1.0, true)
            } else {
                let (r, s) = g.node(orbit::slot_angle(i, n));
                (*id, r, s, false)
            }
        }).collect()
    }
    /// targets blended with the running animation
    pub fn visual(&self) -> (Vec<(u64, R, f64, bool)>, f64) {
        let mut t = self.targets();
        let mut e = 1.0;
        if let Some(Anim::Move { start, dur, from }) = &self.anim {
            e = orbit::ease(start.elapsed().as_secs_f64() / dur.as_secs_f64().max(1e-3));
            for (id, r, _, _) in t.iter_mut() {
                if let Some(f) = from.get(id) {
                    *r = R::lerp(*f, *r, e);
                }
            }
        }
        (t, e)
    }

    /// Change the ring order with an animated transition, then reconfigure clients.
    pub fn transition(&mut self, f: impl FnOnce(&mut Ring)) {
        let before = self.ring.center();
        let (vis, _) = self.visual();
        let from: HashMap<u64, R> = vis.into_iter().map(|(id, r, _, _)| (id, r)).collect();
        f(&mut self.ring);
        let ms = self.cfg.layout.anim_ms;
        self.anim = (ms > 0).then(|| Anim::Move { start: Instant::now(), dur: Duration::from_millis(ms), from });
        self.relayout();
        if self.ring.center() != before {
            self.on_focus_changed();
        }
    }

    /// Send sizes/states to clients and keyboard focus to the centre window.
    pub fn relayout(&mut self) {
        let center = self.ring.center();
        for w in &self.wins {
            let Some(top) = w.window.toplevel() else { continue };
            let is_c = Some(w.id) == center;
            if is_c {
                let r = self.center_rect(w.id);
                top.with_pending_state(|s| {
                    s.size = Some((r.w as i32, r.h as i32).into());
                    s.states.set(xdg_toplevel::State::Activated);
                    for st in [xdg_toplevel::State::TiledLeft, xdg_toplevel::State::TiledRight, xdg_toplevel::State::TiledTop, xdg_toplevel::State::TiledBottom] {
                        s.states.set(st);
                    }
                    if w.size.full { s.states.set(xdg_toplevel::State::Fullscreen) } else { s.states.unset(xdg_toplevel::State::Fullscreen) }
                });
                self.space.map_element(w.window.clone(), (r.x as i32, r.y as i32), true);
            } else {
                top.with_pending_state(|s| {
                    s.states.unset(xdg_toplevel::State::Activated);
                });
            }
            if top.is_initial_configure_sent() {
                top.send_pending_configure();
            }
        }
        let focus = if self.ui.is_some() { None } else { center.and_then(|id| self.win(id)).and_then(|w| w.window.toplevel().map(|t| t.wl_surface().clone())) };
        // an exclusive layer surface (launcher overlay) keeps the keyboard
        let layer_focus = self.output.as_ref().and_then(|o| {
            let map = layer_map_for_output(o);
            let found = map.layers().find(|l| l.can_receive_keyboard_focus() && matches!(l.cached_state().keyboard_interactivity, smithay::wayland::shell::wlr_layer::KeyboardInteractivity::Exclusive)).map(|l| l.wl_surface().clone());
            found
        });
        let target = if self.ui.is_some() { None } else { layer_focus.or(focus) };
        if let Some(k) = self.seat.get_keyboard() {
            if k.current_focus() != target {
                k.set_focus(self, target, Serial::from(smithay::utils::SERIAL_COUNTER.next_serial()));
            }
        }
        self.update_pointer_policy();
        self.dirty = true;
    }

    /// Keyboard-first: the pointer is live only for centre windows tagged requires_pointer.
    pub fn update_pointer_policy(&mut self) {
        self.pointer_live = self.ui.is_none() && self.ring.center().and_then(|id| self.win(id)).map(|w| w.pointer).unwrap_or(false);
    }

    pub fn on_focus_changed(&mut self) {
        // the HUD names the focused binary: refresh it now rather than at the next 2 s tick
        let n = self.ring.order.len();
        let focused = self.ring.center().and_then(|id| self.win(id)).map(|w| if w.app_id.is_empty() { w.title.clone() } else { w.app_id.clone() }).unwrap_or_default();
        if let Some(h) = self.hud.as_mut() {
            h.top_left = format!("◢ SHADOW//RING {:02}/{:02}  ▸ {}", if n > 0 { 1 } else { 0 }, n, if focused.is_empty() { "—" } else { &focused });
        }
        let (app, title) = self.ring.center().and_then(|id| self.win(id)).map(|w| (w.app_id.clone(), w.title.clone())).unwrap_or_default();
        crate::telemetry::send(self, &app);
        if let Some(l) = self.lua.as_mut() {
            l.event("focus", &app, &title);
        }
        self.run_lua_actions();
    }

    // ------------------------------------------------------------------ windows
    fn window_for_surface(&self, s: &WlSurface) -> Option<u64> {
        self.wins.iter().find(|w| w.window.toplevel().map(|t| t.wl_surface() == s).unwrap_or(false)).map(|w| w.id)
    }

    fn read_toplevel_meta(surface: &WlSurface) -> (String, String) {
        with_states(surface, |st| {
            st.data_map.get::<XdgToplevelSurfaceData>().map(|d| {
                let d = d.lock().unwrap();
                (d.app_id.clone().unwrap_or_default(), d.title.clone().unwrap_or_default())
            }).unwrap_or_default()
        })
    }

    pub fn close_center(&mut self) {
        if let Some(t) = self.ring.center().and_then(|id| self.win(id)).and_then(|w| w.window.toplevel().cloned()) {
            t.send_close();
        }
    }

    pub fn run_lua_actions(&mut self) {
        let acts = self.lua.as_mut().map(|l| l.take_actions()).unwrap_or_default();
        for a in acts {
            crate::input::run_action(self, &a);
        }
    }
}

// ---------------------------------------------------------------------- protocol handlers
impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }
    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(p) = get_parent(&root) {
                root = p;
            }
            if let Some(w) = self.wins.iter().find(|w| w.window.toplevel().map(|t| t.wl_surface() == &root).unwrap_or(false)) {
                w.window.on_commit();
                // ring windows are paused: their commits don't cause a redraw
                if Some(w.id) == self.ring.center() {
                    self.dirty = true;
                }
            }
        }
        // initial configure + title/app id changes
        if let Some(id) = self.window_for_surface(surface) {
            let top = self.win(id).and_then(|w| w.window.toplevel().cloned());
            if let Some(top) = top {
                if !top.is_initial_configure_sent() {
                    top.send_configure();
                }
            }
            let (app, title) = Self::read_toplevel_meta(surface);
            if let Some(w) = self.wins.iter_mut().find(|w| w.id == id) {
                if w.app_id != app || w.title != title {
                    let first = w.app_id.is_empty() && !app.is_empty();
                    w.app_id = app.clone();
                    w.title = title.clone();
                    w.pointer = self.cfg.input.pointer_apps.iter().any(|a| a.eq_ignore_ascii_case(&app));
                    if first {
                        if let Some(rule) = self.lua.as_mut().map(|l| l.rule(&app, &title)) {
                            if let Some(p) = rule.preset { w.size.preset = p.min(self.cfg.layout.presets.len() - 1); }
                            if let Some(p) = rule.requires_pointer { w.pointer = p; }
                            if let Some(f) = rule.full { w.size.full = f; }
                        }
                        self.relayout();
                        self.on_focus_changed();
                    }
                    self.dirty = true;
                }
            }
        }
        // popups
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(ref xdg)) = self.popups.find_popup(surface) {
            if !xdg.is_initial_configure_sent() {
                let _ = xdg.send_configure();
            }
            self.dirty = true;
        }
        // layer surfaces
        if let Some(o) = self.output.clone() {
            let mut map = layer_map_for_output(&o);
            if let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).cloned() {
                let initial = with_states(surface, |st| st.data_map.get::<LayerSurfaceData>().map(|d| d.lock().unwrap().initial_configure_sent).unwrap_or(true));
                map.arrange();
                if !initial {
                    layer.layer_surface().send_configure();
                }
                drop(map);
                self.dirty = true;
                self.relayout();
            }
        }
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _: &wl_buffer::WlBuffer) {}
}
impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        if self.greeter_mode {
            surface.send_close();
            return;
        }
        let pid = self.dh.get_client(surface.wl_surface().id()).ok().and_then(|c| c.get_credentials(&self.dh).ok()).map(|c| c.pid);
        surface.with_pending_state(|s| {
            s.decoration_mode = Some(DecoMode::ServerSide);
        });
        self.send_scale(surface.wl_surface());
        let window = Window::new_wayland_window(surface);
        let id = self.next_id;
        self.next_id += 1;
        self.wins.push(Win {
            id, window: window.clone(), size: Size { preset: self.cfg.layout.default_preset, full: false },
            app_id: String::new(), title: String::new(), pid, pointer: false, spark: vec![], cpu_prev: None, rss_mb: 0,
        });
        self.space.map_element(window, (0, 0), true);
        // every new process claims the centre; the previous centre moves to θ₁
        self.transition(|r| r.spawn(id));
        if let Some(l) = self.lua.as_mut() {
            l.event("spawn", "", "");
        }
    }
    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let Some(id) = self.window_for_surface(surface.wl_surface()) else { return };
        if let Some(w) = self.wins.iter().find(|w| w.id == id) {
            self.space.unmap_elem(&w.window);
        }
        self.wins.retain(|w| w.id != id);
        self.render.forget_window(id);
        self.text.forget(&format!("node-{id}-"));
        self.transition(|r| r.remove(id));
    }
    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }
    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|s| {
            s.geometry = positioner.get_geometry();
            s.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }
    fn move_request(&mut self, _: ToplevelSurface, _: wl_seat::WlSeat, _: Serial) {}
    fn resize_request(&mut self, _: ToplevelSurface, _: wl_seat::WlSeat, _: Serial, _: xdg_toplevel::ResizeEdge) {}
    fn grab(&mut self, _: PopupSurface, _: wl_seat::WlSeat, _: Serial) {}
    fn fullscreen_request(&mut self, surface: ToplevelSurface, _: Option<wl_output::WlOutput>) {
        if let Some(id) = self.window_for_surface(surface.wl_surface()) {
            if let Some(w) = self.wins.iter_mut().find(|w| w.id == id) {
                w.size.full = true;
            }
            self.relayout();
        }
    }
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(id) = self.window_for_surface(surface.wl_surface()) {
            if let Some(w) = self.wins.iter_mut().find(|w| w.id == id) {
                w.size.full = false;
            }
            self.relayout();
        }
    }
    fn title_changed(&mut self, surface: ToplevelSurface) {
        let _ = surface;
        self.dirty = true;
    }
}

impl State {
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else { return };
        let Some(w) = self.wins.iter().find(|w| w.window.toplevel().map(|t| t.wl_surface() == &root).unwrap_or(false)) else { return };
        let r = self.center_rect(w.id);
        let (ow, oh) = self.output_size();
        let mut target = Rectangle::new((0, 0).into(), (ow as i32, oh as i32).into());
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= Point::from((r.x as i32, r.y as i32));
        popup.with_pending_state(|s| {
            s.geometry = s.positioner.get_unconstrained_geometry(target);
        });
    }
}

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecoMode::ServerSide));
    }
    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecoMode) {
        // the compositor draws the tactical frame; clients stay borderless
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecoMode::ServerSide));
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| s.decoration_mode = Some(DecoMode::ServerSide));
    }
}

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }
    fn new_layer_surface(&mut self, surface: WlrLayerSurface, _output: Option<wl_output::WlOutput>, _layer: Layer, namespace: String) {
        let Some(o) = self.output.clone() else { return };
        self.send_scale(surface.wl_surface());
        let mut map = layer_map_for_output(&o);
        let _ = map.map_layer(&LayerSurface::new(surface, namespace));
    }
    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        if let Some(o) = self.output.clone() {
            let mut map = layer_map_for_output(&o);
            let layer = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            if let Some(l) = layer {
                map.unmap_layer(&l);
            }
        }
        self.relayout();
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }
    fn cursor_image(&mut self, _: &Seat<Self>, _: smithay::input::pointer::CursorImageStatus) {}
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        // only the focused (centre stage) client may see clipboard / primary selection offers
        let client = focused.and_then(|s| self.dh.get_client(s.id()).ok());
        set_data_device_focus(&self.dh, seat, client.clone());
        set_primary_focus(&self.dh, seat, client);
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}
impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}
impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}
impl PrimarySelectionHandler for State {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}
impl OutputHandler for State {}

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        self.send_scale(&surface);
    }
}

impl State {
    /// Tell a client surface the output scale: exact value via wp_fractional_scale_v1,
    /// and ceil(scale) as wl_surface.preferred_buffer_scale for clients without it, so
    /// legacy clients are downsampled (sharp) rather than upscaled (blurry).
    pub fn send_scale(&self, surface: &WlSurface) {
        let scale = self.scale();
        with_states(surface, |st| {
            send_surface_state(surface, st, scale.ceil() as i32, smithay::utils::Transform::Normal);
            with_fractional_scale(st, |fs| fs.set_preferred_scale(scale));
        });
    }

    /// Apply a new output scale live (config-reload) and re-announce it to every client.
    pub fn set_output_scale(&mut self, scale: f64) {
        let Some(o) = self.output.clone() else { return };
        if (o.current_scale().fractional_scale() - scale).abs() < 1e-6 {
            return;
        }
        o.change_current_state(None, None, Some(smithay::output::Scale::Fractional(scale)), None);
        let surfaces: Vec<WlSurface> = self.wins.iter().filter_map(|w| w.window.toplevel().map(|t| t.wl_surface().clone()))
            .chain(layer_map_for_output(&o).layers().map(|l| l.wl_surface().clone())).collect();
        for s in &surfaces {
            self.send_scale(s);
        }
        self.text.forget("");
        self.relayout();
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }
    fn dmabuf_imported(&mut self, _: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let ok = match &mut self.backend {
            Backend::Winit(w) => w.backend.renderer().import_dmabuf(&dmabuf, None).is_ok(),
            Backend::Udev(u) => u.renderer.as_mut().map(|r: &mut GlesRenderer| r.import_dmabuf(&dmabuf, None).is_ok()).unwrap_or(false),
            Backend::None => false,
        };
        if ok {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

delegate_compositor!(State);
delegate_shm!(State);
delegate_xdg_shell!(State);
delegate_xdg_decoration!(State);
delegate_layer_shell!(State);
delegate_seat!(State);
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_output!(State);
delegate_fractional_scale!(State);
delegate_viewporter!(State);
delegate_dmabuf!(State);
