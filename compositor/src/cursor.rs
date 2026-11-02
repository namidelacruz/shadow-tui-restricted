//! The mouse cursor, drawn by the compositor (on a real login there is no host to draw one).
//! Shapes in Shadow's style — an obsidian arrow with an amber edge, an I-beam for text, a cyan
//! arrow for links, a reticle for move / resize, a ring for busy, a struck circle for "not
//! allowed" — rasterised once per shape and output scale (4×4 supersampled) into a small buffer.
//! A client that sets its own cursor surface (wl_pointer.set_cursor) gets that surface instead;
//! cursor-shape-v1 names map onto these shapes.
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::input::pointer::CursorIcon;
use smithay::utils::Transform;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Shape {
    Arrow,
    Text,
    Hand,
    Cross,
    Busy,
    No,
}

pub fn shape_for(icon: CursorIcon) -> Shape {
    use CursorIcon as C;
    match icon {
        C::Text | C::VerticalText => Shape::Text,
        C::Pointer | C::Grab | C::Grabbing => Shape::Hand,
        C::Crosshair | C::Cell | C::Move | C::AllScroll | C::ColResize | C::RowResize | C::NResize | C::SResize | C::EResize | C::WResize
        | C::NeResize | C::NwResize | C::SeResize | C::SwResize | C::EwResize | C::NsResize | C::NeswResize | C::NwseResize | C::ZoomIn | C::ZoomOut => Shape::Cross,
        C::Wait | C::Progress => Shape::Busy,
        C::NotAllowed | C::NoDrop => Shape::No,
        _ => Shape::Arrow,
    }
}

/// logical size of every shape (the buffer is size × scale pixels)
const SIZE: f32 = 24.0;

/// hotspot in logical px
pub fn hotspot(s: Shape) -> (f32, f32) {
    match s {
        Shape::Arrow | Shape::Hand | Shape::Busy => (1.0, 1.0),
        Shape::Text | Shape::Cross | Shape::No => (12.0, 12.0),
    }
}

const ARROW: [(f32, f32); 7] = [(1.0, 1.0), (1.0, 18.0), (5.2, 14.2), (8.0, 20.5), (10.6, 19.4), (7.9, 13.3), (13.0, 13.3)];

fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
    ((p.0 - a.0 - t * dx).powi(2) + (p.1 - a.1 - t * dy).powi(2)).sqrt()
}

fn inside(p: (f32, f32), poly: &[(f32, f32)]) -> bool {
    let mut c = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < a.0 + (p.1 - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            c = !c;
        }
    }
    c
}

fn edge(p: (f32, f32), poly: &[(f32, f32)]) -> f32 {
    (0..poly.len()).map(|i| seg_dist(p, poly[i], poly[(i + 1) % poly.len()])).fold(f32::MAX, f32::min)
}

/// colour at a logical point: straight RGBA
fn paint(s: Shape, p: (f32, f32), amber: [f32; 3], cyan: [f32; 3], crimson: [f32; 3]) -> [f32; 4] {
    let dark = [0.04, 0.045, 0.055];
    let with = |c: [f32; 3], a: f32| [c[0], c[1], c[2], a];
    let clear = [0.0; 4];
    let arrow = |rim: [f32; 3]| {
        let d = edge(p, &ARROW);
        if inside(p, &ARROW) {
            if d < 1.15 { with(rim, 1.0) } else { with(dark, 0.9) }
        } else if d < 0.55 {
            with(rim, 1.0)
        } else if d < 1.6 {
            with([0.0; 3], 0.35) // soft shadow keeps it readable on bright windows
        } else {
            clear
        }
    };
    let c = (12.0, 12.0);
    let r = ((p.0 - c.0).powi(2) + (p.1 - c.1).powi(2)).sqrt();
    match s {
        Shape::Arrow => arrow(amber),
        Shape::Hand => {
            let a = arrow(cyan);
            // a small target dot by the tip: "this is clickable"
            let dot = ((p.0 - 15.5).powi(2) + (p.1 - 17.5).powi(2)).sqrt();
            if dot < 2.2 { with(cyan, 1.0) } else if dot < 3.2 && a[3] == 0.0 { with(dark, 0.8) } else { a }
        }
        Shape::Busy => {
            let a = arrow(amber);
            let ring = ((p.0 - 16.5).powi(2) + (p.1 - 17.0).powi(2)).sqrt();
            if (ring - 4.0).abs() < 1.1 { with(cyan, 1.0) } else { a }
        }
        Shape::Text => {
            let stem = (p.0 - 12.0).abs() < 0.9 && (4.0..=20.0).contains(&p.1);
            let serif = (p.1 - 4.0).abs() < 0.9 && (p.0 - 12.0).abs() < 3.5 || (p.1 - 20.0).abs() < 0.9 && (p.0 - 12.0).abs() < 3.5;
            let halo = (p.0 - 12.0).abs() < 2.0 && (3.0..=21.0).contains(&p.1) || ((p.1 - 4.0).abs() < 2.0 || (p.1 - 20.0).abs() < 2.0) && (p.0 - 12.0).abs() < 4.6;
            if stem || serif { with(amber, 1.0) } else if halo { with(dark, 0.75) } else { clear }
        }
        Shape::Cross => {
            let gap = r < 2.5;
            let h = (p.1 - 12.0).abs() < 0.9 && (p.0 - 12.0).abs() < 10.0 && !gap;
            let v = (p.0 - 12.0).abs() < 0.9 && (p.1 - 12.0).abs() < 10.0 && !gap;
            let halo = ((p.1 - 12.0).abs() < 2.0 && (p.0 - 12.0).abs() < 11.0) || ((p.0 - 12.0).abs() < 2.0 && (p.1 - 12.0).abs() < 11.0);
            if h || v { with(amber, 1.0) } else if r < 1.0 { with(cyan, 1.0) } else if halo { with(dark, 0.6) } else { clear }
        }
        Shape::No => {
            let ring = (r - 8.0).abs() < 1.4;
            let slash = seg_dist(p, (6.4, 6.4), (17.6, 17.6)) < 1.3;
            if ring || slash { with(crimson, 1.0) } else if r < 10.5 { with(dark, 0.55) } else { clear }
        }
    }
}

#[derive(Default)]
pub struct Cursors {
    cache: HashMap<(Shape, u32), (MemoryRenderBuffer, i32)>,
}

impl Cursors {
    /// the buffer for a shape at this output scale, and its physical edge length
    pub fn get(&mut self, s: Shape, scale: f64, amber: [f32; 3], cyan: [f32; 3], crimson: [f32; 3]) -> (MemoryRenderBuffer, i32) {
        let key = (s, (scale * 100.0).round() as u32);
        if let Some(v) = self.cache.get(&key) {
            return v.clone();
        }
        let px = (SIZE as f64 * scale).round().max(8.0) as i32;
        let buf = raster(s, px, amber, cyan, crimson);
        let b = MemoryRenderBuffer::from_slice(&buf, Fourcc::Argb8888, (px, px), 1, Transform::Normal, None);
        self.cache.insert(key, (b.clone(), px));
        (b, px)
    }
}

/// premultiplied ARGB8888 (little endian B G R A), px × px
pub fn raster(s: Shape, px: i32, amber: [f32; 3], cyan: [f32; 3], crimson: [f32; 3]) -> Vec<u8> {
    let k = SIZE / px as f32;
    let mut out = vec![0u8; (px * px * 4) as usize];
    for y in 0..px {
        for x in 0..px {
            let mut acc = [0.0f32; 4];
            for sy in 0..4 {
                for sx in 0..4 {
                    let p = ((x as f32 + (sx as f32 + 0.5) / 4.0) * k, (y as f32 + (sy as f32 + 0.5) / 4.0) * k);
                    let c = paint(s, p, amber, cyan, crimson);
                    acc[0] += c[0] * c[3];
                    acc[1] += c[1] * c[3];
                    acc[2] += c[2] * c[3];
                    acc[3] += c[3];
                }
            }
            let i = ((y * px + x) * 4) as usize;
            out[i] = (acc[2] / 16.0 * 255.0).round() as u8;
            out[i + 1] = (acc[1] / 16.0 * 255.0).round() as u8;
            out[i + 2] = (acc[0] / 16.0 * 255.0).round() as u8;
            out[i + 3] = (acc[3] / 16.0 * 255.0).round() as u8;
        }
    }
    out
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn shapes_have_ink_where_the_hotspot_is() {
        let (a, c, r) = ([1.0, 0.69, 0.0], [0.0, 0.9, 1.0], [1.0, 0.16, 0.0]);
        for s in [Shape::Arrow, Shape::Text, Shape::Hand, Shape::Cross, Shape::Busy, Shape::No] {
            let px = 36; // scale 1.5
            let b = raster(s, px, a, c, r);
            let ink = b.chunks(4).filter(|p| p[3] > 0).count();
            assert!(ink > 20, "{s:?} is empty");
            let (hx, hy) = hotspot(s);
            let (x, y) = ((hx * 1.5) as i32, (hy * 1.5) as i32);
            let near = (-2..=2).any(|dy| (-2..=2).any(|dx| b[(((y + dy).clamp(0, px - 1) * px + (x + dx).clamp(0, px - 1)) * 4 + 3) as usize] > 0));
            assert!(near || s == Shape::Cross, "{s:?}: nothing near the hotspot");
            // premultiplied: no channel above alpha
            assert!(b.chunks(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]));
        }
        assert_eq!(shape_for(CursorIcon::Text), Shape::Text);
        assert_eq!(shape_for(CursorIcon::Pointer), Shape::Hand);
        assert_eq!(shape_for(CursorIcon::NwseResize), Shape::Cross);
        assert_eq!(shape_for(CursorIcon::Default), Shape::Arrow);
    }
}
