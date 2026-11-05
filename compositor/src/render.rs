//! Scene assembly, front to back (9.0):
//!   lock/greeter → cursor → tutorial → overlay/top layers (the widget columns; hidden under a
//!   fullscreen app) → open apps in gold frames → minimised grid (square tiles: glyph, title,
//!   a still thumbnail) → bottom/background layers → travelling lights → contour background.
//! Minimised apps get no frame callbacks; their tile shows their last frame.
//! Every element keeps a stable id, so an unchanged scene produces zero damage and no redraw;
//! the travelling lights are small quads, so their motion only redraws where they pass.
use crate::config::rgb;
use crate::orbit::R;
use crate::shaders::{self, Shaders};
use crate::state::State;
use smithay::{
    backend::renderer::{
        element::{
            memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            utils::{CropRenderElement, RescaleRenderElement},
            solid::SolidColorRenderElement,
            surface::WaylandSurfaceRenderElement,
            AsRenderElements, Id, Kind,
        },
        gles::{element::PixelShaderElement, GlesPixelProgram, GlesRenderer, Uniform},
        utils::CommitCounter,
    },
    desktop::layer_map_for_output,
    render_elements,
    utils::{Logical, Physical, Point, Rectangle, Scale},
    wayland::shell::wlr_layer::Layer,
};
use std::collections::HashMap;

render_elements! {
    pub El<=GlesRenderer>;
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Shader=PixelShaderElement,
    Solid=SolidColorRenderElement,
    Mem=RescaleRenderElement<MemoryRenderBufferRenderElement<GlesRenderer>>,
    Tile=RescaleRenderElement<CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>>,
    Thumb=RescaleRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
}

struct Slot {
    el: PixelShaderElement,
    area: Rectangle<i32, Logical>,
    vals: Vec<Vec<f32>>,
    alpha: f32,
}

#[derive(Default)]
pub struct RenderCache {
    pub shaders: Option<Shaders>,
    /// per-screen key prefix: each screen keeps its own shader / solid elements (their sizes
    /// differ), so an idle screen sees no damage from the other one
    pub prefix: String,
    slots: HashMap<String, Slot>,
    solids: HashMap<String, (Id, CommitCounter, Rectangle<i32, Physical>, [f32; 4])>,
    pub glyphs: HashMap<(crate::glyphs::G, i32, u8, u8, u8), MemoryRenderBuffer>,
}

impl RenderCache {
    pub fn forget_window(&mut self, id: u64) {
        let tags = [format!("node-{id}"), format!("frame-{id}"), format!("tess-{id}"), format!("tile-{id}"), format!("title-{id}")];
        self.slots.retain(|k, _| !tags.iter().any(|t| k.contains(t.as_str())));
        self.solids.retain(|k, _| !tags.iter().any(|t| k.contains(t.as_str())));
    }

    fn shader(&mut self, key: &str, prog: &GlesPixelProgram, area: Rectangle<i32, Logical>, alpha: f32, uni: Vec<(&'static str, Vec<f32>)>) -> PixelShaderElement {
        let vals: Vec<Vec<f32>> = uni.iter().map(|(_, v)| v.clone()).collect();
        let mk = |u: &[(&'static str, Vec<f32>)]| -> Vec<Uniform<'static>> {
            u.iter().map(|(n, v)| match v.len() {
                1 => Uniform::new(*n, v[0]),
                2 => Uniform::new(*n, [v[0], v[1]]),
                3 => Uniform::new(*n, [v[0], v[1], v[2]]),
                _ => Uniform::new(*n, [v[0], v[1], v[2], v[3]]),
            }).collect()
        };
        let key = &format!("{}{key}", self.prefix);
        match self.slots.get_mut(key) {
            Some(s) if (s.alpha - alpha).abs() < 1e-4 => {
                if s.area != area {
                    s.el.resize(area, None);
                    s.area = area;
                }
                if s.vals != vals {
                    s.el.update_uniforms(mk(&uni));
                    s.vals = vals;
                }
                s.el.clone()
            }
            _ => {
                let el = PixelShaderElement::new(prog.clone(), area, None, alpha, mk(&uni), Kind::Unspecified);
                self.slots.insert(key.into(), Slot { el: el.clone(), area, vals, alpha });
                el
            }
        }
    }

    fn solid(&mut self, key: &str, geo: Rectangle<i32, Physical>, color: [f32; 4]) -> SolidColorRenderElement {
        let e = self.solids.entry(format!("{}{key}", self.prefix)).or_insert_with(|| (Id::new(), CommitCounter::default(), geo, color));
        if e.2 != geo || e.3 != color {
            e.1.increment();
            e.2 = geo;
            e.3 = color;
        }
        SolidColorRenderElement::new(e.0.clone(), geo, e.1, color, Kind::Unspecified)
    }
}

/// the logout animation (seconds) and the session-start reveal
pub const LOGOUT_S: f64 = 1.1;
pub const BOOT_S: f64 = 1.4;
/// travelling lights: (trail, seconds to cross, phase)
const GLINTS: [(usize, f64, f64); 3] = [(1, 46.0, 0.10), (3, 61.0, 0.55), (0, 73.0, 0.82)];
fn bar_px_unused() {}

fn lrect(r: R) -> Rectangle<i32, Logical> {
    Rectangle::new((r.x.round() as i32, r.y.round() as i32).into(), ((r.w.round() as i32).max(1), (r.h.round() as i32).max(1)).into())
}

impl State {
    pub fn ensure_shaders(&mut self, r: &mut GlesRenderer) {
        if self.render.shaders.is_none() {
            match shaders::compile(r) {
                Ok(s) => self.render.shaders = Some(s),
                Err(e) => tracing::error!("shader compile failed: {e:?}"),
            }
        }
    }

    fn colors(&self) -> Vec<(&'static str, Vec<f32>)> {
        let l = &self.cfg.look;
        vec![("u_bg", rgb(&l.obsidian).to_vec()), ("u_amber", rgb(&l.amber).to_vec()), ("u_crimson", rgb(&l.crimson).to_vec())]
    }

    fn text_el(&mut self, r: &mut GlesRenderer, out: &mut Vec<El>, key: &str, s: &str, px: f32, col: [f32; 3], at: (i32, i32), alpha: f32) -> i32 {
        if let Some((buf, (w, _))) = self.text.line(key, s, px, col) {
            // Text is rasterised at physical pixel size. The element's natural
            // size is buffer-px x output-scale, so rescale by 1/scale to map one
            // glyph pixel to one screen pixel at any fractional scale.
            let at = Point::<f64, Physical>::from((at.0 as f64, at.1 as f64));
            if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(r, at, &buf, Some(alpha), None, None, Kind::Unspecified) {
                let inv = Scale::from(1.0 / self.scale());
                out.push(El::Mem(RescaleRenderElement::from_element(e, at.to_i32_round(), inv)));
            }
            return w;
        }
        0
    }

    /// Build the frame. Returns the elements (front to back).
    pub fn elements(&mut self, r: &mut GlesRenderer) -> Vec<El> {
        self.ensure_shaders(r);
        self.render.prefix = if self.cur == 0 { String::new() } else { format!("s{}:", self.cur) };
        let mut out: Vec<El> = vec![];
        let Some(sh) = self.render.shaders.as_ref().map(|s| (s.backdrop.clone(), s.plate.clone(), s.node.clone(), s.bar.clone(), s.background.clone(), s.glint.clone(), s.frame.clone(), s.tess.clone())) else { return out };
        let (p_back, p_plate, _p_node, p_bar, p_bg, p_glint, p_frame, p_tess) = sh;
        let scale = self.scale();
        let sc = Scale::from(scale);
        let (ow, oh) = self.output_size();
        let (pw, ph) = ((ow * scale).round() as i32, (oh * scale).round() as i32);
        let amber = rgb(&self.cfg.look.amber);
        let cyan = rgb(&self.cfg.look.cyan);
        let fpx = self.cfg.look.font_px * scale as f32;
        let boot = self.boot.map(|b| (b.elapsed().as_secs_f64() / BOOT_S).min(1.0)).unwrap_or(1.0) as f32;

        // ---------------------------------------------------------- greeter / lock screen
        if self.ui.is_some() {
            // the card on the focused screen; the others show the figure only
            if self.cur == self.desk.focused || self.desk.screens.len() < 2 {
                self.ui_elements(r, &mut out, &p_frame, &p_bg, &p_glint);
            } else {
                let rv = self.ui.as_ref().map(|u| u.reveal()).unwrap_or(1.0);
                self.background_elements(&mut out, &p_bg, &p_glint, rv, false);
            }
            return out;
        }

        // ---------------------------------------------------------- cursor (front-most)
        self.cursor_elements(r, &mut out);

        // ---------------------------------------------------------- FIELD TRAINING panel
        self.tutorial_elements(r, &mut out, &p_plate);

        // ---------------------------------------------------------- HUD
        let bar_px = self.cfg.hud.height_px;
        if self.cfg.hud.enabled && self.hud_visible && !self.greeter_mode {
            if let Some(h) = self.hud.as_ref().map(|h| (h.top_left.clone(), h.top_right.clone(), h.bottom.clone())) {
                let tpx = (bar_px as f32 * 0.58).max(9.0);
                let ty = ((bar_px as f32 - tpx * 1.3) / 2.0).max(0.0) as i32;
                self.text_el(r, &mut out, "hud-tl", &h.0, tpx, amber, (12, ty), boot);
                let w = self.text.width(&h.1, tpx);
                self.text_el(r, &mut out, "hud-tr", &h.1, tpx, cyan, (pw - w - 12, ty), boot);
                let w = self.text.width(&h.2, tpx);
                self.text_el(r, &mut out, "hud-b", &h.2, tpx, amber, ((pw - w) / 2, ph - bar_px + ty), boot);
            }
            let bh = bar_px as f64 / scale;
            let mut u = self.colors();
            u.push(("u_top", vec![1.0]));
            out.push(El::Shader(self.render.shader("bar-top", &p_bar, lrect(R { x: 0.0, y: 0.0, w: ow, h: bh }), boot, u)));
            let mut u = self.colors();
            u.push(("u_top", vec![0.0]));
            out.push(El::Shader(self.render.shader("bar-bot", &p_bar, lrect(R { x: 0.0, y: oh - bh, w: ow, h: bh }), boot, u)));
        }

        // ---------------------------------------------------------- logout: everything fades into the star
        let lo = self.logout.map(|t| (t.elapsed().as_secs_f64() / LOGOUT_S).min(1.0) as f32);
        let fade = lo.map(|t| (1.0 - t * 1.7).max(0.0)).unwrap_or(1.0);
        let hi = rgb(&self.cfg.look.cyan);

        // ---------------------------------------------------------- overlay + top layers
        let full = self.fullscreen();
        let layers: Vec<(Layer, smithay::desktop::LayerSurface, Rectangle<i32, Logical>)> = self.output().map(|o| {
            let map = layer_map_for_output(o);
            map.layers().filter_map(|l| map.layer_geometry(l).map(|g| (l.layer(), l.clone(), g))).collect()
        }).unwrap_or_default();
        for want in [Layer::Overlay, Layer::Top] {
            // a fullscreen app covers the widget columns (the overlay stays: menus, the hub)
            if want == Layer::Top && full.is_some() {
                continue;
            }
            for (layer, ls, g) in layers.iter().rev() {
                if *layer == want {
                    out.extend(AsRenderElements::<GlesRenderer>::render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(ls, r, g.loc.to_physical_precise_round(sc), sc, fade).into_iter().map(El::Surface));
                }
            }
        }

        // ---------------------------------------------------------- open apps
        let (vis, e) = self.visual();
        let animating = e < 1.0;
        // the focused window shows as focused only on the screen with the keyboard
        let focus = if self.cur == self.desk.focused { self.ring().center() } else { None };
        let ptr = self.pointer_loc;
        // front to back: fullscreen, maximised, focused, then the tiles
        let order = self.stacking();
        let opened: Vec<(u64, R)> = order.iter().filter_map(|id| vis.iter().find(|v| v.0 == *id && v.3).map(|v| (v.0, v.1))).collect();
        let titles = self.cfg.shell.titlebars;
        for (id, rect) in &opened {
            let Some(w) = self.win(*id) else { continue };
            let is_full = Some(*id) == full;
            // kinetic unfolding: the window as a tessellation of cells swinging open / shut
            if let Some(fold) = self.folds.get(id).cloned() {
                let at = fold.at.unwrap_or(*rect);
                let t = fold.t();
                let opening = fold.kind == crate::unfold::Kind::Open;
                let loc = (Point::<f64, Logical>::from((at.x, at.y)) - w.window.geometry().loc.to_f64()).to_physical(sc).to_i32_round();
                for c in crate::unfold::cells(at, fold.origin, t, opening) {
                    if c.p < 0.02 {
                        continue;
                    }
                    let crop = Rectangle::<i32, Physical>::new(((c.r.x * scale).round() as i32, (c.r.y * scale).round() as i32).into(), ((c.r.w * scale).round() as i32, (c.r.h * scale).round() as i32).into());
                    use crate::unfold::Hinge::*;
                    let (hx, hy, sx, sy) = match c.hinge {
                        Left => (c.r.x, c.r.y, c.p, 1.0),
                        Right => (c.r.x + c.r.w, c.r.y, c.p, 1.0),
                        Top => (c.r.x, c.r.y, 1.0, c.p),
                        Bottom => (c.r.x, c.r.y + c.r.h, 1.0, c.p),
                    };
                    let hinge = Point::<i32, Physical>::from(((hx * scale).round() as i32, (hy * scale).round() as i32));
                    let a = (c.p.powf(0.6) as f32) * fade;
                    let els: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = w.window.render_elements(r, loc, sc, a);
                    for e in els {
                        if let Some(cr) = CropRenderElement::from_element(e, sc, crop) {
                            out.push(El::Tile(RescaleRenderElement::from_element(cr, hinge, Scale { x: sx, y: sy })));
                        }
                    }
                }
                // the construction lines over it
                let (cols, rows) = crate::unfold::grid_dims(at);
                let o = (((fold.origin.0 - at.x) / at.w).clamp(-0.5, 1.5) as f32, ((fold.origin.1 - at.y) / at.h).clamp(-0.5, 1.5) as f32);
                let mut u = self.colors();
                u.extend([("u_hi", hi.to_vec()), ("u_cells", vec![cols as f32, rows as f32]), ("u_origin", vec![o.0, o.1]), ("u_front", vec![if opening { t as f32 } else { 1.0 - t as f32 } * 1.2]), ("u_lines", vec![crate::unfold::lines_alpha(t) as f32])]);
                out.push(El::Shader(self.render.shader(&format!("tess-{id}"), &p_tess, lrect(at), fade, u)));
                let mut u = self.colors();
                u.extend([("u_hi", hi.to_vec()), ("u_focus", vec![if Some(*id) == focus { 1.0 } else { 0.0 }]), ("u_fill", vec![0.0]), ("u_hover", vec![0.0])]);
                let fa = if opening { t as f32 } else { 1.0 - t as f32 };
                out.push(El::Shader(self.render.shader(&format!("frame-{id}"), &p_frame, lrect(at.inset(-3.0)), fa * fade, u)));
                continue;
            }
            // the title bar: glyph, name (with its Japanese label), minimise / maximise / close
            if titles && !is_full {
                let (app, title) = (w.app_id.clone(), if w.title.is_empty() { w.app_id.clone() } else { w.title.clone() });
                let maxed = w.size.max;
                self.title_elements(r, &mut out, *id, *rect, &app, &title, Some(*id) == focus, maxed, fade * boot, &p_frame);
            }
            let Some(w) = self.win(*id) else { continue };
            let loc = (Point::<f64, Logical>::from((rect.x, rect.y)) - w.window.geometry().loc.to_f64()).to_physical(sc).to_i32_round();
            let els: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = w.window.render_elements(r, loc, sc, fade);
            out.extend(els.into_iter().map(El::Surface));
            if !is_full {
                let f = if Some(*id) == focus { 1.0 } else { 0.0 };
                let m = 3.0;
                let tb = if titles { crate::orbit::TITLE_H } else { 0.0 };
                let fr = R { x: rect.x - m, y: rect.y - m - tb, w: rect.w + 2.0 * m, h: rect.h + 2.0 * m + tb };
                let mut u = self.colors();
                u.extend([("u_hi", hi.to_vec()), ("u_focus", vec![f]), ("u_fill", vec![0.0]), ("u_hover", vec![0.0])]);
                out.push(El::Shader(self.render.shader(&format!("frame-{id}"), &p_frame, lrect(fr), boot * fade, u)));
            }
        }

        // ---------------------------------------------------------- minimised grid
        let gap = self.cfg.layout.gap as f64;
        let tiles: Vec<(u64, R)> = vis.iter().filter(|v| !v.3).map(|v| (v.0, v.1)).collect();
        if let (_, max, Some(band)) = self.grid() {
            if !tiles.is_empty() && max > 0.0 {
                // scroll hint on the band's right edge
                let n = self.ring().min.len();
                let s = format!("{n} MINIMISED · SCROLL");
                let px = fpx * 0.72;
                let w = self.text.width(&s, px);
                self.text_el(r, &mut out, "grid-hint", &s, px, [0.56, 0.5, 0.38], (((band.x + band.w) * scale) as i32 - w, ((band.y - 16.0) * scale) as i32), 0.8 * fade);
            }
        }
        for (id, rect) in tiles {
            let Some((app, title)) = self.win(id).map(|w| (w.app_id.clone(), if w.title.is_empty() { w.app_id.clone() } else { w.title.clone() })) else { continue };
            let hover = rect.contains(ptr.x, ptr.y);
            // title under the tile
            let px = fpx * 0.74;
            let mut name: String = title.chars().take(24).collect();
            while self.text.width(&name, px) as f64 > rect.w * scale && name.pop().is_some() {}
            let tw = self.text.width(&name, px);
            let ty = ((rect.y + rect.h + 5.0) * scale) as i32;
            self.text_el(r, &mut out, &format!("tile-{id}-t"), &name, px, if hover { hi } else { [0.78, 0.71, 0.55] }, ((((rect.x + rect.w / 2.0) * scale) as i32) - tw / 2, ty), fade);
            // the app's glyph in the corner
            let gpx = ((rect.w * 0.30).clamp(22.0, 48.0) * scale).round() as i32;
            let g = crate::glyphs::for_app(&app);
            let gb = self.glyph_buf(g, gpx, if hover { hi } else { amber });
            let gat = Point::<f64, Physical>::from((((rect.x + rect.w) * scale) - gpx as f64 - 6.0 * scale, ((rect.y + rect.h) * scale) - gpx as f64 - 6.0 * scale));
            if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(r, gat, &gb, Some(fade), None, None, Kind::Unspecified) {
                out.push(El::Mem(RescaleRenderElement::from_element(el, gat.to_i32_round(), Scale::from(1.0))));
            }
            // a still thumbnail of its last frame, fitted inside
            if let Some(w) = self.win(id) {
                let geo = w.window.geometry();
                if geo.size.w > 0 && geo.size.h > 0 {
                    let pad = 8.0;
                    let k = ((rect.w - 2.0 * pad) / geo.size.w as f64).min((rect.h - 2.0 * pad) / geo.size.h as f64).min(1.0);
                    let (tw, th) = (geo.size.w as f64 * k, geo.size.h as f64 * k);
                    let tl = Point::<f64, Logical>::from((rect.x + (rect.w - tw) / 2.0 - geo.loc.x as f64 * k, rect.y + (rect.h - th) / 2.0 - geo.loc.y as f64 * k));
                    // drawn at full size at the tile's corner, then shrunk around that corner
                    let at = Point::<i32, Physical>::from(((tl.x * scale).round() as i32, (tl.y * scale).round() as i32));
                    let els: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = w.window.render_elements(r, at, sc, 0.55 * fade);
                    out.extend(els.into_iter().map(|e| El::Thumb(RescaleRenderElement::from_element(e, at, Scale::from(k)))));
                }
            }
            let mut u = self.colors();
            u.extend([("u_hi", hi.to_vec()), ("u_focus", vec![0.0]), ("u_fill", vec![1.0]), ("u_hover", vec![if hover { 1.0 } else { 0.0 }])]);
            out.push(El::Shader(self.render.shader(&format!("tile-{id}"), &p_frame, lrect(rect), boot * fade, u)));
        }
        let _ = (animating, gap);

        // ---------------------------------------------------------- bottom + background layers
        for want in [Layer::Bottom, Layer::Background] {
            for (layer, ls, g) in layers.iter().rev() {
                if *layer == want {
                    out.extend(AsRenderElements::<GlesRenderer>::render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(ls, r, g.loc.to_physical_precise_round(sc), sc, fade).into_iter().map(El::Surface));
                }
            }
        }

        // ---------------------------------------------------------- background + travelling light
        let reveal = match lo { Some(t) => 1.0 - t, None => boot.max(0.0) };
        self.background_elements(&mut out, &p_bg, &p_glint, reveal, full.is_some());
        if self.boot.map(|b| b.elapsed().as_secs_f64() > BOOT_S).unwrap_or(false) {
            self.boot = None;
        }
        if lo.map(|t| t >= 1.0).unwrap_or(false) {
            self.signal.stop();
        }
        let _ = (&p_back, &p_bar, bar_px_unused());
        out
    }

    /// A window's title bar (10.0, Windows 11 layout in the glyph style): the app's glyph and its
    /// bilingual label on the left, the title, then minimise · maximise / restore · close.
    #[allow(clippy::too_many_arguments)]
    fn title_elements(&mut self, r: &mut GlesRenderer, out: &mut Vec<El>, id: u64, content: R, app: &str, title: &str, focused: bool, maxed: bool, a: f32, p_frame: &GlesPixelProgram) {
        let scale = self.scale();
        let px = |v: f64| (v * scale).round() as i32;
        let (bar, btns) = crate::orbit::title_bar(content);
        let amber = rgb(&self.cfg.look.amber);
        let hi = rgb(&self.cfg.look.cyan);
        let crimson = rgb(&self.cfg.look.crimson);
        let dim = [0.56, 0.5, 0.38];
        let ptr = self.pointer_loc;
        let fpx = self.cfg.look.font_px * scale as f32 * 0.86;
        let ty = px(bar.y + (bar.h - fpx as f64 / scale * 1.25) / 2.0);
        // buttons first (front), then the text, then the strip behind them
        for (b, rr) in btns {
            use crate::orbit::TitleBtn::*;
            let hover = rr.contains(ptr.x, ptr.y);
            if hover {
                let c = if b == Close { [crimson[0], crimson[1], crimson[2], 0.9] } else { [amber[0] * 0.25, amber[1] * 0.25, amber[2] * 0.25, 0.9] };
                out.push(El::Solid(self.render.solid(&format!("title-{id}-h{b:?}"), Rectangle::new((px(rr.x), px(rr.y)).into(), (px(rr.w), px(rr.h)).into()), c)));
            }
            let col = if hover && b == Close { [1.0, 0.95, 0.9] } else if hover { hi } else if focused { amber } else { dim };
            // drawn as strokes: — ▢ (⧉ when maximised) ✕, centred in the button
            let (cx, cy) = (rr.x + rr.w / 2.0, rr.y + rr.h / 2.0);
            let s = 5.0;
            let t = (1.2 * scale).max(1.0).round() as i32;
            let mut line = |k: &str, x0: f64, y0: f64, w: f64, h: f64| {
                let geo = Rectangle::new((px(x0), px(y0)).into(), (px(w).max(t), px(h).max(t)).into());
                out.push(El::Solid(self.render.solid(&format!("title-{id}-{k}"), geo, [col[0], col[1], col[2], a])));
            };
            match b {
                Min => line("min", cx - s, cy, 2.0 * s, 0.0),
                Max if maxed => {
                    // two overlapping squares: restore down
                    line("mx1", cx - s + 2.0, cy - s, 2.0 * s - 2.0, 0.0);
                    line("mx2", cx + s, cy - s, 0.0, 2.0 * s - 2.0);
                    line("mx3", cx - s, cy - s + 2.0, 2.0 * s - 2.0, 0.0);
                    line("mx4", cx - s, cy + s, 2.0 * s - 2.0, 0.0);
                    line("mx5", cx - s, cy - s + 2.0, 0.0, 2.0 * s - 2.0);
                    line("mx6", cx + s - 2.0, cy - s + 2.0, 0.0, 2.0 * s - 2.0);
                }
                Max => {
                    line("mx1", cx - s, cy - s, 2.0 * s, 0.0);
                    line("mx3", cx - s, cy + s, 2.0 * s, 0.0);
                    line("mx5", cx - s, cy - s, 0.0, 2.0 * s);
                    line("mx6", cx + s, cy - s, 0.0, 2.0 * s);
                }
                Close => {
                    // an ✕ from short diagonal steps
                    for i in 0..7 {
                        let d = -s + i as f64 * (2.0 * s / 6.0);
                        line(&format!("x{i}a"), cx + d - 0.6, cy + d - 0.6, 1.2, 1.2);
                        line(&format!("x{i}b"), cx + d - 0.6, cy - d - 0.6, 1.2, 1.2);
                    }
                }
                Bar => {}
            }
        }
        // glyph + bilingual label (端末 TERMINAL) + title
        let g = crate::glyphs::for_app(app);
        let gpx = px(bar.h * 0.62);
        let gb = self.glyph_buf(g, gpx, if focused { amber } else { dim });
        let gat = Point::<f64, Physical>::from((px(bar.x + 10.0) as f64, px(bar.y) as f64 + (px(bar.h) - gpx) as f64 / 2.0));
        if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(r, gat, &gb, Some(a), None, None, Kind::Unspecified) {
            out.push(El::Mem(RescaleRenderElement::from_element(el, gat.to_i32_round(), Scale::from(1.0))));
        }
        let label = crate::glyphs::bilingual(g);
        let lx = px(bar.x + 10.0) + gpx + px(8.0);
        let lw = self.text_el(r, out, &format!("d:title-{id}-l"), &label, fpx, if focused { amber } else { dim }, (lx, ty), a);
        let room = px(bar.w - 3.0 * crate::orbit::TITLE_BTN_W - 24.0) - (lx + lw - px(bar.x));
        if room > px(40.0) && !title.is_empty() && !title.eq_ignore_ascii_case(app) {
            let mut t: String = title.chars().take(120).collect();
            while self.text.width(&t, fpx) > room && t.pop().is_some() {}
            self.text_el(r, out, &format!("title-{id}-t"), &format!("· {t}"), fpx, if focused { [0.92, 0.88, 0.77] } else { dim }, (lx + lw + px(6.0), ty), a);
        }
        // the strip: a gold frame shader with fill, a hair brighter when focused
        let mut u = self.colors();
        u.extend([("u_hi", hi.to_vec()), ("u_focus", vec![if focused { 1.0 } else { 0.0 }]), ("u_fill", vec![1.0]), ("u_hover", vec![0.0])]);
        out.push(El::Shader(self.render.shader(&format!("title-{id}"), p_frame, lrect(bar), a, u)));
    }

    /// The login / lock card under the Vitruvian figure: SHADOW in Shadow Glyph, the fields, the
    /// desktop switch and the power row, each a gold-framed box the mouse can click.
    fn ui_elements(&mut self, r: &mut GlesRenderer, out: &mut Vec<El>, p_frame: &GlesPixelProgram, p_bg: &GlesPixelProgram, p_glint: &GlesPixelProgram) {
        let scale = self.scale();
        let (ow, oh) = self.output_size();
        let amber = rgb(&self.cfg.look.amber);
        let hi = rgb(&self.cfg.look.cyan);
        let crimson = rgb(&self.cfg.look.crimson);
        let text = [0.92, 0.88, 0.77];
        let dim = [0.56, 0.5, 0.38];
        let fpx = self.cfg.look.font_px * scale as f32;
        let (a, reveal, greeter, lines, sess, field, user, npass, status, busy) = {
            let u = self.ui.as_ref().unwrap();
            (u.text_alpha(), u.reveal(), u.mode == crate::ui::Mode::Greeter, u.lines(), u.session_name(), u.field, u.user.clone(), u.pass.chars().count(), u.status.clone(), u.busy)
        };
        let ptr = self.pointer_loc;
        let mut buttons: Vec<(&'static str, R)> = vec![];
        let mut extra: Vec<(&'static str, R)> = vec![];
        // cursor on top
        if a > 0.0 {
            self.ui_cursor(r, out);
        }
        let px = |v: f64| (v * scale) as i32;
        // the card: centred, under the figure's middle
        let cw = (ow * 0.34).clamp(360.0, 560.0);
        let cx = (ow - cw) / 2.0;
        let mut y = oh * 0.52;
        let lh = fpx as f64 / scale * 1.9;
        let mut boxed = |out: &mut Vec<El>, this: &mut State, key: &'static str, rr: R, focus: bool| {
            let hover = rr.contains(ptr.x, ptr.y);
            let mut u = this.colors();
            u.extend([("u_hi", hi.to_vec()), ("u_focus", vec![if focus { 1.0 } else { 0.0 }]), ("u_fill", vec![0.9]), ("u_hover", vec![if hover { 1.0 } else { 0.0 }])]);
            out.push(El::Shader(this.render.shader(&format!("ui-box-{key}"), p_frame, lrect(rr), a, u)));
            buttons.push((key, rr));
        };
        // title
        let tpx = fpx * 2.6;
        let title = "SHADOW";
        let tw = self.text.width_d(title, tpx);
        self.text_el(r, out, "d:ui-title", title, tpx, amber, ((px(ow) - tw) / 2, px(y)), a);
        y += tpx as f64 / scale * 1.25;
        let sub = if greeter { lines.get(1).map(|l| l.0.clone()).unwrap_or_default() } else { lines.get(1).map(|l| l.0.clone()).unwrap_or_default() };
        let spx = fpx * 0.78;
        let sw = self.text.width(&sub, spx);
        self.text_el(r, out, "ui-sub", &sub, spx, if greeter { crimson } else { dim }, ((px(ow) - sw) / 2, px(y)), a * 0.9);
        y += lh * 1.1;
        // fields
        let fh = lh * 1.25;
        if greeter {
            let rr = R { x: cx, y, w: cw, h: fh };
            let caret = if field == 0 { "_" } else { "" };
            self.text_el(r, out, "d:ui-user-l", "USER", fpx * 0.8, dim, (px(cx + 14.0), px(y + fh * 0.28)), a);
            self.text_el(r, out, "ui-user", &format!("{user}{caret}"), fpx, text, (px(cx + 110.0), px(y + fh * 0.22)), a);
            boxed(out, self, "user", rr, field == 0);
            y += fh + 10.0;
        }
        let rr = R { x: cx, y, w: cw, h: fh };
        let caret = if field == 1 { "_" } else { "" };
        self.text_el(r, out, "d:ui-pass-l", "PASS", fpx * 0.8, dim, (px(cx + 14.0), px(y + fh * 0.28)), a);
        self.text_el(r, out, "ui-pass", &format!("{}{caret}", "◆".repeat(npass.min(32))), fpx * 0.8, hi, (px(cx + 110.0), px(y + fh * 0.3)), a);
        boxed(out, self, "pass", rr, field == 1);
        y += fh + 10.0;
        if greeter {
            // the desktop switch
            let rr = R { x: cx, y, w: cw, h: fh };
            let name = sess.to_uppercase();
            let nw = self.text.width_d(&name, fpx * 0.9);
            self.text_el(r, out, "d:ui-sess", &name, fpx * 0.9, hi, ((px(ow) - nw) / 2, px(y + fh * 0.26)), a);
            self.text_el(r, out, "d:ui-sess-l", "<", fpx, amber, (px(cx + 14.0), px(y + fh * 0.22)), a);
            let aw = self.text.width_d(">", fpx);
            self.text_el(r, out, "d:ui-sess-r", ">", fpx, amber, (px(cx + cw - 14.0) - aw, px(y + fh * 0.22)), a);
            boxed(out, self, "sess", rr, false);
            extra.push(("sess-prev", R { x: cx, y, w: cw * 0.25, h: fh }));
            extra.push(("sess-next", R { x: cx + cw * 0.75, y, w: cw * 0.25, h: fh }));
            y += fh + 6.0;
        }
        // status
        let st = if busy { "AUTHENTICATING…".to_string() } else { status.0.clone() };
        if !st.is_empty() {
            let sw = self.text.width(&st, fpx * 0.8);
            self.text_el(r, out, "ui-status", &st, fpx * 0.8, if status.1 == 2 { crimson } else { hi }, ((px(ow) - sw) / 2, px(y + 4.0)), a);
        }
        y += lh;
        // the power row (login only): glyph buttons
        if greeter {
            let row = [("emergency", crate::glyphs::G::Emergency, "TERMINAL UI"), ("suspend", crate::glyphs::G::Suspend, "SUSPEND"), ("reboot", crate::glyphs::G::Restart, "RESTART"), ("poweroff", crate::glyphs::G::Power, "SHUT DOWN")];
            let bw = (cw - 3.0 * 10.0) / 4.0;
            let bh = fh * 1.6;
            for (i, (key, g, label)) in row.iter().enumerate() {
                let rr = R { x: cx + i as f64 * (bw + 10.0), y, w: bw, h: bh };
                let hover = rr.contains(ptr.x, ptr.y);
                let col = if *key == "emergency" { if hover { [1.0, 0.55, 0.4] } else { crimson } } else if hover { hi } else { amber };
                let gpx = (bh * 0.42 * scale) as i32;
                let gb = self.glyph_buf(*g, gpx, col);
                let at = Point::<f64, Physical>::from(((rr.x + rr.w / 2.0) * scale - gpx as f64 / 2.0, (rr.y + 6.0) * scale));
                if let Ok(el) = MemoryRenderBufferRenderElement::from_buffer(r, at, &gb, Some(a), None, None, Kind::Unspecified) {
                    out.push(El::Mem(RescaleRenderElement::from_element(el, at.to_i32_round(), Scale::from(1.0))));
                }
                let lw = self.text.width_d(label, fpx * 0.62);
                self.text_el(r, out, &format!("d:ui-b-{key}"), label, fpx * 0.62, col, (px(rr.x + rr.w / 2.0) - lw / 2, px(rr.y + bh - fpx as f64 / scale * 1.2)), a);
                boxed(out, self, key, rr, false);
            }
            y += bh + 10.0;
        }
        // hint line
        let hint = lines.last().map(|l| l.0.clone()).unwrap_or_default();
        let hw = self.text.width(&hint, fpx * 0.66);
        self.text_el(r, out, "ui-hint", &hint, fpx * 0.66, dim, ((px(ow) - hw) / 2, px(y + 6.0)), a * 0.8);
        drop(boxed);
        // the arrows of the switch first (they sit inside its box)
        extra.extend(buttons);
        if let Some(u) = self.ui.as_mut() {
            u.buttons = extra;
        }
        self.background_elements(out, p_bg, p_glint, reveal, false);
    }

    /// the arrow cursor on the login / lock screen
    fn ui_cursor(&mut self, r: &mut GlesRenderer, out: &mut Vec<El>) {
        if self.cursor_typing_hidden {
            return;
        }
        let (ow, oh) = self.output_size();
        if self.pointer_loc.x < 0.0 || self.pointer_loc.y < 0.0 || self.pointer_loc.x >= ow || self.pointer_loc.y >= oh {
            return;
        }
        let scale = self.scale();
        let (amber, cyan, crimson) = (rgb(&self.cfg.look.amber), rgb(&self.cfg.look.cyan), rgb(&self.cfg.look.crimson));
        let (buf, _) = self.cursors.get(crate::cursor::Shape::Arrow, scale, amber, cyan, crimson);
        let p = self.pointer_loc.to_physical(Scale::from(scale));
        let at = Point::<f64, Physical>::from((p.x - scale, p.y - scale));
        if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(r, at, &buf, None, None, None, Kind::Cursor) {
            out.push(El::Mem(RescaleRenderElement::from_element(e, at.to_i32_round(), Scale::from(1.0))));
        }
    }

    /// the contour background and the lights travelling its trail lines (none while an app is
    /// fullscreen, or with look.motion = "off")
    fn background_elements(&mut self, out: &mut Vec<El>, p_bg: &GlesPixelProgram, p_glint: &GlesPixelProgram, reveal: f32, covered: bool) {
        let (ow, oh) = self.output_size();
        let scale = self.scale();
        let (pw, ph) = (ow * scale, oh * scale);
        let hi = rgb(&self.cfg.look.cyan);
        if !covered && self.cfg.look.motion != "off" && self.ui.as_ref().map(|u| u.mode != crate::ui::Mode::Lock).unwrap_or(true) {
            let t = self.start.elapsed().as_secs_f64();
            for (k, (trail, period, phase)) in GLINTS.iter().enumerate() {
                let head = ((t / period + phase).fract()) * 1.3 - 0.15;
                let tail = 0.16;
                // the quad: from the tail's end to just past the head, around the line
                let (u0, u1) = ((head - tail).max(-0.05), (head + 0.03).min(1.05));
                if u1 <= 0.0 || u0 >= 1.0 {
                    continue;
                }
                let (mut y0, mut y1) = (f64::MAX, f64::MIN);
                for i in 0..=16 {
                    let u = u0 + (u1 - u0) * i as f64 / 16.0;
                    let y = crate::shaders::trail_y(*trail, u) * ph;
                    y0 = y0.min(y);
                    y1 = y1.max(y);
                }
                let m = 30.0 * scale;
                let (qx, qy) = ((u0 * pw - m).max(0.0), (y0 - m).max(0.0));
                let (qw, qh) = (((u1 * pw + m).min(pw) - qx).max(1.0), ((y1 + m).min(ph) - qy).max(1.0));
                let rect = R { x: qx / scale, y: qy / scale, w: qw / scale, h: qh / scale };
                let mut u = self.colors();
                u.extend([("u_hi", hi.to_vec()), ("u_trail", vec![*trail as f32]), ("u_head", vec![head as f32]), ("u_tail", vec![tail as f32]),
                          ("u_screen", vec![(rect.x * scale) as f32, (rect.y * scale) as f32, pw as f32, ph as f32])]);
                out.push(El::Shader(self.render.shader(&format!("glint-{k}"), p_glint, lrect(rect), reveal.clamp(0.0, 1.0), u)));
            }
        }
        // the hidden hub's star sits in the middle of the app area
        let a = self.area();
        let star = if self.ui.is_some() { (pw / 2.0, ph * 0.40, ph.min(pw) * 0.30) } else { ((a.x + a.w / 2.0) * scale, (a.y + a.h / 2.0) * scale, (a.w.min(a.h) * 0.40) * scale) };
        let mut u = self.colors();
        u.extend([("u_hi", hi.to_vec()), ("u_seed", vec![0.37]), ("u_star", vec![star.0 as f32, star.1 as f32]), ("u_starr", vec![star.2 as f32]), ("u_reveal", vec![reveal.clamp(0.0, 1.0)])]);
        out.push(El::Shader(self.render.shader("background", p_bg, lrect(R { x: 0.0, y: 0.0, w: ow, h: oh }), 1.0, u)));
    }

    /// a glyph icon as a tinted buffer (cached per glyph, size and colour)
    pub fn glyph_buf(&mut self, g: crate::glyphs::G, px: i32, col: [f32; 3]) -> MemoryRenderBuffer {
        let key = (g, px, (col[0] * 255.0) as u8, (col[1] * 255.0) as u8, (col[2] * 255.0) as u8);
        if let Some(b) = self.render.glyphs.get(&key) {
            return b.clone();
        }
        let m = crate::glyphs::raster(g, px as usize, (px as f32 / 24.0).max(1.3));
        let mut buf = vec![0u8; (px * px * 4) as usize];
        for (i, v) in m.iter().enumerate() {
            let a = v.clamp(0.0, 1.0);
            buf[i * 4] = (col[2] * a * 255.0) as u8;
            buf[i * 4 + 1] = (col[1] * a * 255.0) as u8;
            buf[i * 4 + 2] = (col[0] * a * 255.0) as u8;
            buf[i * 4 + 3] = (a * 255.0) as u8;
        }
        let b = MemoryRenderBuffer::from_slice(&buf, smithay::backend::allocator::Fourcc::Argb8888, (px, px), 1, smithay::utils::Transform::Normal, None);
        self.render.glyphs.insert(key, b.clone());
        b
    }

    fn cursor_elements(&mut self, r: &mut GlesRenderer, out: &mut Vec<El>) {
        use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
        use smithay::reexports::wayland_server::Resource;
        if !self.pointer_live || self.cursor_typing_hidden || self.ui.is_some() {
            return;
        }
        // only on the screen the pointer is on (it is in this screen's coordinates here)
        let (ow, oh) = self.output_size();
        let p = self.pointer_loc;
        if p.x < 0.0 || p.y < 0.0 || p.x >= ow || p.y >= oh {
            return;
        }
        let scale = self.scale();
        let sc = Scale::from(scale);
        match self.cursor_status.clone() {
            CursorImageStatus::Hidden => {}
            CursorImageStatus::Surface(surface) if surface.is_alive() => {
                let hot = smithay::wayland::compositor::with_states(&surface, |st| {
                    st.data_map.get::<CursorImageSurfaceData>().map(|d| d.lock().unwrap().hotspot).unwrap_or_default()
                });
                let at = (self.pointer_loc - hot.to_f64()).to_physical(sc).to_i32_round();
                let els: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
                    smithay::backend::renderer::element::surface::render_elements_from_surface_tree(r, &surface, at, sc, 1.0, Kind::Cursor);
                out.extend(els.into_iter().map(El::Surface));
            }
            st => {
                let icon = match st { CursorImageStatus::Named(i) => i, _ => smithay::input::pointer::CursorIcon::Default };
                let shape = crate::cursor::shape_for(icon);
                let (amber, cyan, crimson) = (rgb(&self.cfg.look.amber), rgb(&self.cfg.look.cyan), rgb(&self.cfg.look.crimson));
                let (buf, _) = self.cursors.get(shape, scale, amber, cyan, crimson);
                let (hx, hy) = crate::cursor::hotspot(shape);
                let p = self.pointer_loc.to_physical(sc);
                let at = Point::<f64, Physical>::from((p.x - hx as f64 * scale, p.y - hy as f64 * scale));
                if let Ok(e) = MemoryRenderBufferRenderElement::from_buffer(r, at, &buf, None, None, None, Kind::Cursor) {
                    out.push(El::Mem(RescaleRenderElement::from_element(e, at.to_i32_round(), Scale::from(1.0))));
                }
            }
        }
    }

    /// FIELD TRAINING: a glass panel above the bottom edge; one hotkey at a time
    fn tutorial_elements(&mut self, r: &mut GlesRenderer, out: &mut Vec<El>, p_plate: &GlesPixelProgram) {
        if self.cur != self.desk.focused.min(self.desk.screens.len().saturating_sub(1)) {
            return; // on the screen with the keyboard
        }
        self.tut_buttons.clear();
        if self.ui.is_some() {
            return;
        }
        let Some((lines, rise, flash)) = self.tutorial.as_ref().map(|t| (t.lines(&self.cfg.input.modifier), t.rise(), t.flash())) else { return };
        let scale = self.scale();
        let (ow, oh) = self.output_size();
        let (pw, ph) = ((ow * scale).round() as i32, (oh * scale).round() as i32);
        let amber = rgb(&self.cfg.look.amber);
        let cyan = rgb(&self.cfg.look.cyan);
        let fpx = self.cfg.look.font_px * scale as f32;
        let size = |i: usize| if i == 1 { fpx * 1.5 } else if i == 2 { fpx * 1.25 } else { fpx * 0.95 };
        let lh = |i: usize| (size(i) * 1.55) as i32;
        let text_w = lines.iter().enumerate().map(|(i, (l, _))| self.text.width(l, size(i))).max().unwrap_or(0);
        let w_px = (text_w as f64 + 64.0 * scale).max(560.0 * scale).min(pw as f64 - 32.0 * scale);
        let h_px: f64 = lines.iter().enumerate().map(|(i, _)| lh(i) as f64).sum::<f64>() + 40.0 * scale + fpx as f64 * 1.6;
        // top centre: the hub keeps its widgets in the corners and along the bottom edge
        let x0 = (pw as f64 - w_px) / 2.0;
        let y0 = self.bar_h() * scale + 28.0 * scale - (1.0 - rise as f64) * 60.0 * scale;
        let a = rise;
        // text, top to bottom
        let mut y = (y0 + 18.0 * scale) as i32;
        for (i, (l, kind)) in lines.iter().enumerate() {
            let col = match kind { 1 => amber, 2 => cyan, 3 => [0.93, 0.91, 0.84], _ => [0.66, 0.64, 0.58] };
            let col = if i == 1 && flash > 0.0 { cyan } else { col };
            let px = size(i);
            let w = self.text.width(l, px);
            // the key line is the call to action: a keycap frame around it
            if i == 2 && !l.is_empty() {
                let pad = (8.0 * scale) as i32;
                let kx = (pw - w) / 2 - pad;
                let kw = w + 2 * pad;
                let kh = (px * 1.4) as i32;
                let c = [amber[0], amber[1], amber[2], 0.85 * a];
                for (k, rect) in [("t", (kx, y - 2, kw, 2)), ("b", (kx, y + kh - 2, kw, 2)), ("l", (kx, y - 2, 2, kh)), ("r", (kx + kw - 2, y - 2, 2, kh))] {
                    out.push(El::Solid(self.render.solid(&format!("tut-cap-{k}"), Rectangle::new((rect.0, rect.1).into(), (rect.2, rect.3).into()), c)));
                }
            }
            self.text_el(r, out, &format!("tut-{i}"), l, px, col, ((pw - w) / 2, y), a);
            y += lh(i);
        }
        // buttons: SKIP › and CLOSE ✕ — clickable (input.rs reads tut_buttons)
        let done = self.tutorial.as_ref().map(|t| t.done).unwrap_or(true);
        let bpx = fpx * 0.9;
        let by = (y0 + h_px - fpx as f64 * 1.5 - 10.0 * scale) as i32;
        let mut bx = (x0 + w_px - 18.0 * scale) as i32;
        for (name, label) in [("close", "CLOSE ✕"), ("skip", "SKIP ›")] {
            if name == "skip" && done {
                continue;
            }
            let w = self.text.width(label, bpx);
            bx -= w;
            let hover = {
                let lp = self.pointer_loc;
                let rr = R { x: bx as f64 / scale - 6.0, y: by as f64 / scale - 4.0, w: w as f64 / scale + 12.0, h: bpx as f64 * 1.5 / scale + 8.0 };
                let o = self.origin();
                self.tut_buttons.push((name, R { x: rr.x + o.x, y: rr.y + o.y, ..rr }));
                lp.x >= rr.x && lp.x < rr.x + rr.w && lp.y >= rr.y && lp.y < rr.y + rr.h
            };
            self.text_el(r, out, &format!("tut-b-{name}"), label, bpx, if hover { cyan } else { amber }, (bx, by), a * if hover { 1.0 } else { 0.8 });
            bx -= (22.0 * scale) as i32;
        }
        // glass plate with a cyan pulse on success
        let rect = R { x: x0 / scale, y: y0 / scale, w: w_px / scale, h: h_px / scale };
        let edge = [cyan[0], cyan[1], cyan[2], (0.9 * flash * (1.0 - flash) * 4.0).min(0.9) * a];
        if edge[3] > 0.01 {
            let (x, yy, w, h) = (x0 as i32, y0 as i32, w_px as i32, h_px as i32);
            out.push(El::Solid(self.render.solid("tut-flash-t", Rectangle::new((x, yy).into(), (w, 3).into()), edge)));
            out.push(El::Solid(self.render.solid("tut-flash-b", Rectangle::new((x, yy + h - 3).into(), (w, 3).into()), edge)));
        }
        let mut u = self.colors();
        u.extend([("u_opacity", vec![0.97]), ("u_blur", vec![0.0]), ("u_screen", vec![x0 as f32, y0 as f32, pw as f32, ph as f32])]);
        out.push(El::Shader(self.render.shader("tut-back", p_plate, lrect(rect), a, u)));
    }

    /// true while something on screen is still moving
    pub fn animating(&mut self) -> bool {
        if let Some(crate::state::Anim::Move { start, dur, .. }) = &self.anim {
            if start.elapsed() >= *dur {
                self.anim = None;
                self.dirty = true; // one last frame at rest
            }
        }
        let tut = match self.tutorial.as_mut() {
            Some(t) => {
                let moving = t.tick();
                if t.done && t.expired() {
                    crate::input::close_tutorial(self);
                    false
                } else {
                    moving || t.done
                }
            }
            None => false,
        };
        let folding = self.step_folds();
        self.anim.is_some() || self.boot.is_some() || self.logout.is_some() || folding || tut || self.ui.as_ref().map(|u| u.moving()).unwrap_or(false)
    }

    /// frame callbacks: open apps and layer surfaces (minimised apps stay paused)
    pub fn send_frames(&self, output: &smithay::output::Output) {
        let t = self.start.elapsed();
        if self.ui.is_none() {
            for id in self.ring().order.iter() {
                if let Some(w) = self.win(*id) {
                    w.window.send_frame(output, t, Some(std::time::Duration::ZERO), |_, _| Some(output.clone()));
                }
            }
        }
        let map = layer_map_for_output(output);
        for l in map.layers() {
            l.send_frame(output, t, Some(std::time::Duration::ZERO), |_, _| Some(output.clone()));
        }
        // an animated client cursor keeps animating
        if let smithay::input::pointer::CursorImageStatus::Surface(s) = &self.cursor_status {
            smithay::desktop::utils::send_frames_surface_tree(s, output, t, Some(std::time::Duration::ZERO), |_, _| Some(output.clone()));
        }
    }
}
