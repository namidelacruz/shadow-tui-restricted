//! Scene assembly, front to back:
//!   lock/greeter text → pointer → HUD → overlay/top layers → centre window → obsidian plate →
//!   ring nodes (near to far) → bottom/background layers → backdrop.
//! Ring windows are not drawn from their buffers at all: a background node is a wireframe box
//! with the title, PID and a low-frequency CPU sparkline, and its client gets no frame callbacks.
//! Every element keeps a stable id, so an unchanged scene produces zero damage and no redraw.
use crate::config::rgb;
use crate::orbit::R;
use crate::shaders::{self, Shaders};
use crate::state::State;
use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement,
            utils::RescaleRenderElement,
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
    slots: HashMap<String, Slot>,
    solids: HashMap<String, (Id, CommitCounter, Rectangle<i32, Physical>, [f32; 4])>,
}

impl RenderCache {
    pub fn forget_window(&mut self, id: u64) {
        let p = format!("node-{id}");
        self.slots.retain(|k, _| !k.starts_with(&p));
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
        let e = self.solids.entry(key.into()).or_insert_with(|| (Id::new(), CommitCounter::default(), geo, color));
        if e.2 != geo || e.3 != color {
            e.1.increment();
            e.2 = geo;
            e.3 = color;
        }
        SolidColorRenderElement::new(e.0.clone(), geo, e.1, color, Kind::Unspecified)
    }
}

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
        let mut out: Vec<El> = vec![];
        let Some(sh) = self.render.shaders.as_ref().map(|s| (s.backdrop.clone(), s.plate.clone(), s.node.clone(), s.bar.clone())) else { return out };
        let (p_back, p_plate, p_node, p_bar) = sh;
        let scale = self.scale();
        let sc = Scale::from(scale);
        let (ow, oh) = self.output_size();
        let (pw, ph) = ((ow * scale).round() as i32, (oh * scale).round() as i32);
        let amber = rgb(&self.cfg.look.amber);
        let crimson = rgb(&self.cfg.look.crimson);
        let cyan = rgb(&self.cfg.look.cyan);
        let fpx = self.cfg.look.font_px * scale as f32;
        let boot = self.boot.map(|b| (b.elapsed().as_secs_f64() / 0.6).min(1.0)).unwrap_or(1.0) as f32;

        // ---------------------------------------------------------- greeter / lock screen
        if self.ui.is_some() {
            let (globe, rot) = { let u = self.ui.as_ref().unwrap(); (u.globe(), u.rot) };
            let lines = self.ui.as_ref().unwrap().lines();
            let mut y = (ph as f64 * 0.66) as i32;
            // glass backing behind the text block (same plate shader as centre stage)
            let block_w = lines.iter().map(|(s, _)| self.text.width(s, fpx * 1.35)).max().unwrap_or(0) as f64 + 48.0 * scale;
            let block_h = lines.len() as f64 * fpx as f64 * 1.7 + fpx as f64 * 1.6;
            let back = R { x: (pw as f64 - block_w) / 2.0 / scale, y: (y as f64 - fpx as f64 * 0.8) / scale, w: block_w / scale, h: block_h / scale };
            let ta = self.ui.as_ref().unwrap().text_alpha();
            for (i, (s, kind)) in lines.iter().enumerate() {
                let col = match kind { 1 => amber, 2 => crimson, 3 => cyan, _ => [0.75, 0.72, 0.62] };
                let px = if i == 0 { fpx * 1.35 } else { fpx };
                let w = self.text.width(s, px);
                let a = self.ui.as_ref().unwrap().text_alpha();
                self.text_el(r, &mut out, &format!("ui-{i}"), s, px, col, ((pw - w) / 2, y), a);
                y += (px * 1.7) as i32;
            }
            if ta > 0.0 {
                let mut u = self.colors();
                u.extend([("u_opacity", vec![0.8]), ("u_blur", vec![1.0]), ("u_screen", vec![(back.x * scale) as f32, (back.y * scale) as f32, pw as f32, ph as f32])]);
                out.push(El::Shader(self.render.shader("ui-back", &p_plate, lrect(back), ta, u)));
            }
            let mut u = self.colors();
            u.extend([("u_grid", vec![1.0]), ("u_globe", vec![globe]), ("u_rot", vec![rot]), ("u_phase", vec![1.0])]);
            out.push(El::Shader(self.render.shader("backdrop", &p_back, lrect(R { x: 0.0, y: 0.0, w: ow, h: oh }), 1.0, u)));
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

        // ---------------------------------------------------------- overlay + top layers
        let layers: Vec<(Layer, smithay::desktop::LayerSurface, Rectangle<i32, Logical>)> = self.output.as_ref().map(|o| {
            let map = layer_map_for_output(o);
            map.layers().filter_map(|l| map.layer_geometry(l).map(|g| (l.layer(), l.clone(), g))).collect()
        }).unwrap_or_default();
        for want in [Layer::Overlay, Layer::Top] {
            for (layer, ls, g) in layers.iter().rev() {
                if *layer == want {
                    out.extend(AsRenderElements::<GlesRenderer>::render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(ls, r, g.loc.to_physical_precise_round(sc), sc, 1.0).into_iter().map(El::Surface));
                }
            }
        }

        // ---------------------------------------------------------- windows
        let (vis, e) = self.visual();
        let animating = e < 1.0;
        let center = self.ring.center();
        if let Some((id, rect, _, _)) = vis.iter().find(|v| v.3).cloned() {
            let fade = if animating { ((e - 0.6) / 0.4).clamp(0.0, 1.0) as f32 } else { 1.0 };
            if fade > 0.0 {
                if let Some(w) = self.win(id) {
                    let loc = (Point::<f64, Logical>::from((rect.x, rect.y)) - w.window.geometry().loc.to_f64()).to_physical(sc).to_i32_round();
                    let els: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = w.window.render_elements(r, loc, sc, fade);
                    out.extend(els.into_iter().map(El::Surface));
                }
            }
            let full = self.win(id).map(|w| w.size.full).unwrap_or(false);
            let m = if full { 0.0 } else { 6.0 };
            let rect = R { x: rect.x - m, y: rect.y - m, w: rect.w + 2.0 * m, h: rect.h + 2.0 * m };
            let mut u = self.colors();
            u.extend([
                ("u_opacity", vec![self.cfg.look.plate_opacity]),
                ("u_blur", vec![if self.cfg.look.blur { 1.0 } else { 0.0 }]),
                ("u_screen", vec![(rect.x * scale) as f32, (rect.y * scale) as f32, pw as f32, ph as f32]),
            ]);
            if !animating {
                out.push(El::Shader(self.render.shader("plate", &p_plate, lrect(rect), boot, u)));
            } else {
                // the incoming window materialises from its wireframe
                let mut nu = self.colors();
                nu.extend([("u_opacity", vec![0.9]), ("u_hl", vec![0.0])]);
                out.push(El::Shader(self.render.shader(&format!("node-{id}"), &p_node, lrect(rect), 1.0, nu)));
                if fade > 0.0 {
                    out.push(El::Shader(self.render.shader("plate", &p_plate, lrect(rect), fade, u)));
                }
            }
        }
        // ring nodes, nearest first (front to back)
        let mut ring: Vec<(u64, R, f64)> = vis.iter().filter(|v| !v.3).map(|v| (v.0, v.1, v.2)).collect();
        ring.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        let node_op = self.cfg.look.node_opacity;
        for (id, rect, depth) in ring {
            let lines = self.win(id).map(|w| {
                let name = if w.title.is_empty() { w.app_id.clone() } else { w.title.clone() };
                let name: String = name.chars().take(34).collect();
                let pid = w.pid.map(|p| format!("PID {p}")).unwrap_or_else(|| "PID ?".into());
                let spark: String = w.spark.iter().map(|v| ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'][((v / 100.0 * 7.0).round() as usize).min(7)]).collect();
                (name, format!("{pid} · {}M", w.rss_mb), format!("{spark} {:>3.0}%", w.spark.last().copied().unwrap_or(0.0)))
            });
            let a = (boot * if animating { 1.0 } else { (0.55 + 0.45 * depth as f32).min(1.0) }).min(1.0);
            if let Some((n, p, s)) = lines {
                let px = fpx * 0.82;
                let (x0, y0) = (((rect.x + 8.0) * scale) as i32, ((rect.y + 6.0) * scale) as i32);
                if rect.h * scale > (px * 4.6) as f64 && !(animating && Some(id) == center) {
                    let lh = (px * 1.45) as i32;
                    self.text_el(r, &mut out, &format!("node-{id}-t"), &n, px, amber, (x0, y0), a);
                    self.text_el(r, &mut out, &format!("node-{id}-p"), &p, px, [0.62, 0.6, 0.52], (x0, y0 + lh), a * 0.9);
                    self.text_el(r, &mut out, &format!("node-{id}-s"), &s, px, cyan, (x0, y0 + 2 * lh), a * 0.9);
                }
            }
            let mut u = self.colors();
            u.extend([("u_opacity", vec![node_op]), ("u_hl", vec![0.0])]);
            out.push(El::Shader(self.render.shader(&format!("node-{id}"), &p_node, lrect(rect), a, u)));
        }

        // ---------------------------------------------------------- bottom + background layers
        for want in [Layer::Bottom, Layer::Background] {
            for (layer, ls, g) in layers.iter().rev() {
                if *layer == want {
                    out.extend(AsRenderElements::<GlesRenderer>::render_elements::<WaylandSurfaceRenderElement<GlesRenderer>>(ls, r, g.loc.to_physical_precise_round(sc), sc, 1.0).into_iter().map(El::Surface));
                }
            }
        }

        // ---------------------------------------------------------- backdrop
        let mut u = self.colors();
        u.extend([("u_grid", vec![if self.cfg.look.grid { 1.0 } else { 0.0 }]), ("u_globe", vec![0.0]), ("u_rot", vec![0.0]), ("u_phase", vec![1.0])]);
        out.push(El::Shader(self.render.shader("backdrop", &p_back, lrect(R { x: 0.0, y: 0.0, w: ow, h: oh }), 1.0, u)));
        if self.boot.map(|b| b.elapsed().as_secs_f64() > 0.6).unwrap_or(false) {
            self.boot = None;
        }
        out
    }

    fn cursor_elements(&mut self, r: &mut GlesRenderer, out: &mut Vec<El>) {
        use smithay::input::pointer::{CursorImageStatus, CursorImageSurfaceData};
        use smithay::reexports::wayland_server::Resource;
        if !self.pointer_live || self.cursor_typing_hidden || self.ui.is_some() {
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
                self.tut_buttons.push((name, rr));
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
        self.anim.is_some() || self.boot.is_some() || tut || self.ui.as_ref().map(|u| u.moving()).unwrap_or(false)
    }

    /// frame callbacks: only the centre window and layer surfaces (ring windows stay paused)
    pub fn send_frames(&self, output: &smithay::output::Output) {
        let t = self.start.elapsed();
        if let Some(w) = self.ring.center().and_then(|id| self.win(id)) {
            if self.ui.is_none() {
                w.window.send_frame(output, t, Some(std::time::Duration::ZERO), |_, _| Some(output.clone()));
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
