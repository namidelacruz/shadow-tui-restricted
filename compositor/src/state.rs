//! Compositor state and Wayland protocol handlers.
use crate::config::{self, Config};
use crate::orbit::{self, Ring, Size, R};
use crate::ui::Ui;
use smithay::{
    backend::{
        allocator::dmabuf::Dmabuf,
        renderer::{gles::GlesRenderer, utils::on_commit_buffer_handler, ImportDma},
    },
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_fractional_scale, delegate_layer_shell, delegate_output,
    delegate_primary_selection, delegate_seat, delegate_shm, delegate_viewporter, delegate_xdg_decoration, delegate_xdg_shell,
    delegate_cursor_shape, delegate_idle_notify, delegate_xdg_activation, delegate_pointer_constraints, delegate_relative_pointer,
    desktop::{find_popup_root_surface, get_popup_toplevel_coords, layer_map_for_output, LayerSurface, PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy, Space, Window, WindowSurfaceType},
    input::{keyboard::XkbConfig, pointer::{CursorImageStatus, Focus}, Seat, SeatHandler, SeatState},
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
        cursor_shape::CursorShapeManagerState,
        pointer_constraints::{with_pointer_constraint, PointerConstraintsHandler, PointerConstraintsState},
        relative_pointer::RelativePointerManagerState,
        idle_notify::{IdleNotifierHandler, IdleNotifierState},
        tablet_manager::TabletSeatHandler,
        xdg_activation::{XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData},
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
    pub activation_state: XdgActivationState,
    pub cursor_shape_state: CursorShapeManagerState,
    pub idle_notifier: IdleNotifierState<State>,
    pub pointer_constraints_state: PointerConstraintsState,
    pub relative_pointer_state: RelativePointerManagerState,
    /// the surface whose pointer lock / confinement is active (games, 3D viewports)
    pub constrained: Option<WlSurface>,
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
    /// what the client under the pointer asked for (a named shape, its own surface, or hidden)
    pub cursor_status: CursorImageStatus,
    pub cursors: crate::cursor::Cursors,
    /// hidden while typing (input.hide_cursor_when_typing); back on the next move
    pub cursor_typing_hidden: bool,
    /// a layer surface (hub widget, menu) that got the keyboard by a click (OnDemand)
    pub layer_kb_focus: Option<WlSurface>,
    /// FIELD TRAINING, the hotkey tutorial (tutorial.rs)
    pub tutorial: Option<crate::tutorial::Tutorial>,
    /// the tutorial panel's clickable buttons: (name, logical rect)
    pub tut_buttons: Vec<(&'static str, R)>,
    /// fractional scroll steps toward the next ring turn
    pub ring_scroll: f64,
    /// scroll offset of the minimised grid, logical px
    pub grid_scroll: f64,
    /// index into layout.presets: the main tile's share
    pub main_preset: usize,
    /// the logout animation is running; the session ends when it finishes
    pub logout: Option<Instant>,
    /// on mains power (the background light runs faster)
    pub on_ac: bool,
    /// windows unfolding / folding right now (unfold.rs)
    pub folds: HashMap<u64, crate::unfold::Fold>,
    /// where the next window unfolds from (a widget's centre, sent with `origin` over IPC), and when
    pub pending_origin: Option<((f64, f64), Instant)>,
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
        let activation_state = XdgActivationState::new::<State>(&dh);
        let cursor_shape_state = CursorShapeManagerState::new::<State>(&dh);
        let idle_notifier = IdleNotifierState::<State>::new(&dh, handle.clone());
        let pointer_constraints_state = PointerConstraintsState::new::<State>(&dh);
        let relative_pointer_state = RelativePointerManagerState::new::<State>(&dh);
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
            activation_state, cursor_shape_state, idle_notifier, pointer_constraints_state, relative_pointer_state, constrained: None,
            popups: PopupManager::default(), seat,
            space: Space::default(), output: None, backend: Backend::None,
            wins: vec![], ring: Ring::default(), next_id: 1, anim: None, hud_visible: !crate::config::hub_enabled(), hud: None, hud_extra: vec![],
            refresh_hz: 60, wanted_refresh: None,
            ui: None, greeter_mode: greeter, boot: (!greeter).then(Instant::now),
            pointer_loc: (0.0, 0.0).into(), pointer_live: false,
            cursor_status: CursorImageStatus::default_named(), cursors: Default::default(), cursor_typing_hidden: false,
            layer_kb_focus: None, tutorial: None, tut_buttons: vec![], ring_scroll: 0.0, grid_scroll: 0.0, main_preset: 1, logout: None, on_ac: true, folds: HashMap::new(), pending_origin: None, suppressed_keys: vec![], key_times: vec![], last_input: Instant::now(),
            text, render: Default::default(), lua: None, dirty: true, mem_high_seen: 0,
            binds: vec![], ui_tx: None, exit_code: 0,
        }
    }

    // ------------------------------------------------------------------ geometry
    pub fn output_size(&self) -> (f64, f64) {
        self.output.as_ref().and_then(|o| self.space.output_geometry(o)).map(|g| (g.size.w as f64, g.size.h as f64)).unwrap_or((1280.0, 800.0))
    }
    /// the output's connector name ("eDP-1"; "winit" nested)
    pub fn output_name(&self) -> String {
        self.output.as_ref().map(|o| o.name()).unwrap_or_else(|| "none".into())
    }
    pub fn scale(&self) -> f64 {
        self.output.as_ref().map(|o| o.current_scale().fractional_scale()).unwrap_or(1.0)
    }
    /// HUD bar height in logical px
    pub fn bar_h(&self) -> f64 {
        if self.cfg.hud.enabled && self.hud_visible && !self.greeter_mode { self.cfg.hud.height_px as f64 / self.scale() } else { 0.0 }
    }
    /// The app area: the screen minus the widget columns (layer-shell exclusive zones) and a gap.
    pub fn area(&self) -> R {
        let (w, h) = self.output_size();
        let z = self.output.as_ref().map(|o| layer_map_for_output(o).non_exclusive_zone());
        let b = self.bar_h();
        let r = match z {
            Some(z) if z.size.w > 0 && z.size.h > 0 => R { x: z.loc.x as f64, y: z.loc.y as f64 + b, w: z.size.w as f64, h: z.size.h as f64 - 2.0 * b },
            _ => R { x: 0.0, y: b, w, h: h - 2.0 * b },
        };
        r.inset(self.cfg.layout.gap as f64)
    }
    /// (tiles area, minimised-grid band): the band runs along the bottom while apps are open
    /// and takes the whole area when none are
    pub fn areas(&self) -> (R, Option<R>) {
        let a = self.area();
        if self.ring.min.is_empty() {
            return (a, None);
        }
        if self.ring.order.is_empty() {
            return (a, Some(a));
        }
        let g = self.cfg.layout.gap as f64;
        let bh = (self.cfg.layout.tile_px as f64 + 26.0).min(a.h * 0.4);
        (R { h: (a.h - bh - g).max(60.0), ..a }, Some(R { y: a.y + a.h - bh, h: bh, ..a }))
    }
    pub fn win(&self, id: u64) -> Option<&Win> {
        self.wins.iter().find(|w| w.id == id)
    }
    pub fn main_share(&self) -> f64 {
        let l = &self.cfg.layout;
        l.presets.get(self.main_preset.min(l.presets.len().saturating_sub(1))).copied().unwrap_or(60) as f64 / 100.0
    }
    /// The minimised grid: every tile and how far it can scroll (tiles outside the band are
    /// left out of `targets`).
    pub fn grid(&self) -> (Vec<(u64, R)>, f64, Option<R>) {
        let (_, band) = self.areas();
        let Some(band) = band else { return (vec![], 0.0, None) };
        let label = 22.0;
        let inner = R { h: band.h, ..band };
        let cell = R { h: (inner.h - label).max(40.0), ..inner };
        let side = (self.cfg.layout.tile_px as f64).min(cell.h).max(24.0);
        let gap = self.cfg.layout.gap as f64;
        // one row (apps open): a strip that scrolls sideways; the whole area: a grid scrolling down
        let (rects, max) = if cell.h < 2.0 * side + gap + label {
            let n = self.ring.min.len();
            let content = n as f64 * (side + gap) - gap;
            let max = (content - cell.w).max(0.0);
            let s = self.grid_scroll.clamp(0.0, max);
            let x0 = cell.x + ((cell.w - content) / 2.0).max(0.0);
            ((0..n).map(|i| R { x: (x0 + i as f64 * (side + gap) - s).round(), y: cell.y, w: side, h: side }).collect(), max)
        } else {
            orbit::grid(cell, self.ring.min.len(), self.cfg.layout.tile_px as f64, gap + label, self.grid_scroll)
        };
        (self.ring.min.iter().copied().zip(rects).collect(), max, Some(band))
    }
    /// where every window is drawn right now, without animation: (id, rect, scale, open?).
    /// Open apps get their tile (or the whole screen in fullscreen); minimised apps their square
    /// in the grid, when it is inside the band.
    pub fn targets(&self) -> Vec<(u64, R, f64, bool)> {
        let (tiles_area, _) = self.areas();
        let (ow, oh) = self.output_size();
        let tiles = orbit::tile(tiles_area, self.ring.order.len(), self.cfg.layout.gap as f64, self.main_share());
        let mut v: Vec<(u64, R, f64, bool)> = self.ring.order.iter().zip(tiles).map(|(id, r)| {
            let full = self.win(*id).map(|w| w.size.full).unwrap_or(false);
            (*id, if full { R { x: 0.0, y: 0.0, w: ow, h: oh } } else { r }, 1.0, true)
        }).collect();
        let (grid, _, band) = self.grid();
        if let Some(b) = band {
            for (id, r) in grid {
                // visible while most of it is inside the band
                let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                if cy >= b.y && cy <= b.y + b.h && cx >= b.x && cx <= b.x + b.w {
                    v.push((id, r, 1.0, false));
                }
            }
        }
        v
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
    /// the window that is fullscreen right now (it covers the widget columns too)
    pub fn fullscreen(&self) -> Option<u64> {
        self.ring.order.iter().copied().find(|id| self.win(*id).map(|w| w.size.full).unwrap_or(false))
    }

    /// Change the window order with an animated transition, then reconfigure clients.
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

    /// Send sizes/states to clients and keyboard focus to the focused app.
    pub fn relayout(&mut self) {
        let center = self.ring.center();
        let targets = self.targets();
        for w in &self.wins {
            let Some(top) = w.window.toplevel() else { continue };
            let open = targets.iter().find(|t| t.0 == w.id && t.3).map(|t| t.1);
            if let Some(r) = open {
                let focused = Some(w.id) == center;
                top.with_pending_state(|s| {
                    s.size = Some(((r.w as i32).max(1), (r.h as i32).max(1)).into());
                    if focused { s.states.set(xdg_toplevel::State::Activated); } else { s.states.unset(xdg_toplevel::State::Activated); }
                    for st in [xdg_toplevel::State::TiledLeft, xdg_toplevel::State::TiledRight, xdg_toplevel::State::TiledTop, xdg_toplevel::State::TiledBottom] {
                        s.states.set(st);
                    }
                    if w.size.full { s.states.set(xdg_toplevel::State::Fullscreen) } else { s.states.unset(xdg_toplevel::State::Fullscreen) }
                });
                self.space.map_element(w.window.clone(), (r.x as i32, r.y as i32), focused);
            } else {
                // minimised: keeps its last buffer for the grid tile
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
        // a layer surface clicked into (OnDemand) keeps the keyboard while it is still mapped
        let clicked = self.layer_kb_focus.clone().filter(|s| {
            self.output.as_ref().map(|o| layer_map_for_output(o).layers().any(|l| l.wl_surface() == s && l.can_receive_keyboard_focus())).unwrap_or(false)
        });
        if clicked.is_none() {
            self.layer_kb_focus = None;
        }
        let target = if self.ui.is_some() { None } else { layer_focus.or(clicked).or(focus) };
        if let Some(k) = self.seat.get_keyboard() {
            if k.current_focus() != target {
                k.set_focus(self, target, Serial::from(smithay::utils::SERIAL_COUNTER.next_serial()));
            }
        }
        self.update_pointer_policy();
        self.dirty = true;
    }

    /// minimise: the window folds up toward the grid band, then joins the grid
    pub fn minimize(&mut self, id: u64) {
        let ms = self.cfg.layout.unfold_ms;
        if ms == 0 || !self.ring.order.contains(&id) {
            self.transition(|r| { r.minimize(id); });
            return;
        }
        let at = self.targets().into_iter().find(|t| t.0 == id).map(|t| t.1);
        let a = self.area();
        self.folds.insert(id, crate::unfold::Fold::new(crate::unfold::Kind::Minimize, (a.x + a.w / 2.0, a.y + a.h), at, ms * 3 / 4));
        self.dirty = true;
    }
    /// restore: the window unfolds out of its grid tile
    pub fn restore(&mut self, id: u64) {
        let max = self.cfg.layout.max_open;
        let from = self.targets().into_iter().find(|t| t.0 == id && !t.3).map(|t| (t.1.x + t.1.w / 2.0, t.1.y + t.1.h / 2.0));
        self.transition(|r| { r.focus_with(id, max); });
        if self.cfg.layout.unfold_ms > 0 {
            let o = from.unwrap_or_else(|| self.take_origin());
            self.folds.insert(id, crate::unfold::Fold::new(crate::unfold::Kind::Open, o, None, self.cfg.layout.unfold_ms));
        }
    }

    /// The pointer works everywhere except on the lock / greeter screen. In input.pointer = "apps"
    /// the centre window only gets it when tagged (pointer_apps / a Lua rule); Shadow's layers and
    /// the ring always do (input.rs decides per surface).
    pub fn update_pointer_policy(&mut self) {
        self.pointer_live = self.ui.is_none();
    }
    /// may the centre window receive pointer events?
    pub fn center_takes_pointer(&self) -> bool {
        self.cfg.input.pointer != "apps" || self.ring.center().and_then(|id| self.win(id)).map(|w| w.pointer).unwrap_or(false)
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

    /// Mod+Q: the focused app folds up toward the middle, then is asked to close
    pub fn close_center(&mut self) {
        let Some(id) = self.ring.center() else { return };
        let ms = self.cfg.layout.unfold_ms;
        if ms == 0 {
            if let Some(t) = self.win(id).and_then(|w| w.window.toplevel().cloned()) {
                t.send_close();
            }
            return;
        }
        let at = self.targets().into_iter().find(|t| t.0 == id).map(|t| t.1);
        let a = self.area();
        self.folds.insert(id, crate::unfold::Fold::new(crate::unfold::Kind::Close, (a.x + a.w / 2.0, a.y + a.h / 2.0), at, ms * 3 / 4));
        self.dirty = true;
    }

    /// where a new or restored window unfolds from: a widget that asked (origin over IPC, 3 s)
    /// or the hidden hub in the middle of the app area
    pub fn take_origin(&mut self) -> (f64, f64) {
        if let Some((p, t)) = self.pending_origin.take() {
            if t.elapsed() < Duration::from_secs(3) {
                return p;
            }
        }
        let a = self.area();
        (a.x + a.w / 2.0, a.y + a.h / 2.0)
    }

    /// finish folds that are done: closes and minimises happen now; true while any is running
    pub fn step_folds(&mut self) -> bool {
        let done: Vec<(u64, crate::unfold::Kind)> = self.folds.iter().filter(|(_, f)| f.done()).map(|(id, f)| (*id, f.kind)).collect();
        for (id, kind) in done {
            self.folds.remove(&id);
            match kind {
                crate::unfold::Kind::Close => {
                    if let Some(t) = self.win(id).and_then(|w| w.window.toplevel().cloned()) {
                        t.send_close();
                    }
                }
                crate::unfold::Kind::Minimize => {
                    let anim = self.cfg.layout.anim_ms;
                    self.cfg.layout.anim_ms = 0;
                    self.transition(|r| { r.minimize(id); });
                    self.cfg.layout.anim_ms = anim;
                }
                crate::unfold::Kind::Open => {}
            }
            self.dirty = true;
        }
        !self.folds.is_empty()
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
                // open apps redraw; minimised ones only refresh their grid tile on the next frame
                if self.ring.order.contains(&w.id) {
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
        // a new app takes the main tile; past max_open the oldest open app is minimised
        let max = self.cfg.layout.max_open;
        self.transition(|r| { r.spawn(id, max); });
        if self.cfg.layout.unfold_ms > 0 {
            let o = self.take_origin();
            self.folds.insert(id, crate::unfold::Fold::new(crate::unfold::Kind::Open, o, None, self.cfg.layout.unfold_ms));
        }
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
        self.folds.remove(&id);
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
    /// menus (right-click, combo boxes): the popup holds the pointer and keyboard until a click
    /// outside it or Escape dismisses it
    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<State>::from_resource(&seat) else { return };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        let Ok(mut grab) = self.popups.grab_popup::<State>(root, kind, &seat, serial) else { return };
        if let Some(k) = seat.get_keyboard() {
            if k.is_grabbed() && !(k.has_grab(serial) || k.has_grab(grab.previous_serial().unwrap_or(serial))) {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            k.set_focus(self, grab.current_grab(), serial);
            k.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(p) = seat.get_pointer() {
            if p.is_grabbed() && !(p.has_grab(serial) || p.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial()))) {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            p.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }
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
        let r = self.targets().into_iter().find(|t| t.0 == w.id).map(|t| t.1).unwrap_or(self.area());
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
    fn cursor_image(&mut self, _: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
        self.dirty = true;
    }
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
delegate_cursor_shape!(State);
delegate_idle_notify!(State);
delegate_xdg_activation!(State);
delegate_pointer_constraints!(State);
delegate_relative_pointer!(State);

/// pointer lock / confinement (games, Blender's viewport): granted while the surface has the pointer
impl PointerConstraintsHandler for State {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &smithay::input::pointer::PointerHandle<Self>) {
        if pointer.current_focus().as_ref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |c| if let Some(c) = c { c.activate() });
            self.constrained = Some(surface.clone());
        }
    }
    fn cursor_position_hint(&mut self, _: &WlSurface, _: &smithay::input::pointer::PointerHandle<Self>, _: Point<f64, Logical>) {}
}

impl TabletSeatHandler for State {}

impl IdleNotifierHandler for State {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
        &mut self.idle_notifier
    }
}

/// xdg-activation: an app asks for one of its windows to come forward (a link opened in the
/// browser, a second launch of a single-instance app) — it moves to the centre of the ring
impl XdgActivationHandler for State {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.activation_state
    }
    fn request_activation(&mut self, token: XdgActivationToken, data: XdgActivationTokenData, surface: WlSurface) {
        // only tokens younger than 10 s, as other compositors do
        if data.timestamp.elapsed() < Duration::from_secs(10) {
            if let Some(id) = self.window_for_surface(&surface) {
                self.transition(|r| { r.focus(id); });
            }
        }
        self.activation_state.remove_token(&token);
    }
}
