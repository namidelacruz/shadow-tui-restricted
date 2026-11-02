//! Tactical holographic shaders (GLSL ES 1.00, drawn through Smithay's GLES renderer).
//!
//! Every shader is a pure function of its uniforms. With no time uniform changing, a frame never
//! changes, so the damage tracker skips the redraw and the panel can drop into self-refresh.
//! `v_coords` runs 0..1 across the element and `size` is its size in physical pixels.
use smithay::backend::renderer::gles::{GlesError, GlesPixelProgram, GlesRenderer, UniformName, UniformType};

const COMMON: &str = r#"
precision highp float;
varying vec2 v_coords;
uniform vec2 size;
uniform float alpha;
uniform vec3 u_bg;
uniform vec3 u_amber;
uniform vec3 u_crimson;

float lineaa(float d, float w) { return 1.0 - smoothstep(w * 0.5, w * 0.5 + 1.0, d); }

// perspective floor grid in screen space (p in pixels, s = full size): horizon at 58 %,
// amber minor lines, crimson every fifth; `blur` widens and softens the lines.
vec3 floorgrid(vec2 p, vec2 s, float blur) {
    float hz = s.y * 0.58;
    if (p.y <= hz + 2.0) return vec3(0.0);
    float dy = p.y - hz;
    float z = s.y * 0.55 / dy;              // distance along the floor
    float x = (p.x - s.x * 0.5) / dy * 0.55; // lateral position
    float k = 3.2;
    vec2 g = vec2(x * k, z * k);
    vec2 f = abs(fract(g - 0.5) - 0.5);
    vec2 px = vec2(0.55 * k / dy, s.y * 0.55 * k / (dy * dy)); // d(g)/d(pixel)
    float w = 1.0 + blur * 6.0;
    float lx = lineaa(f.x / px.x, w);
    float lz = lineaa(f.y / px.y, w);
    vec2 spacing = 1.0 / max(px, vec2(1e-4)); // pixels between lines
    lx *= smoothstep(5.0, 16.0, spacing.x);
    lz *= smoothstep(5.0, 16.0, spacing.y);
    vec2 cell = floor(g + 0.5);
    float major = (mod(cell.x, 5.0) == 0.0 ? 1.0 : 0.0);
    float majz = (mod(cell.y, 5.0) == 0.0 ? 1.0 : 0.0);
    float fade = clamp(dy / (s.y * 0.42), 0.0, 1.0) * smoothstep(0.0, s.y * 0.05, dy);
    fade = sqrt(fade) * (1.0 - blur * 0.55);
    vec3 c = u_amber * 0.42 * max(lx * (1.0 - major), lz * (1.0 - majz));
    c += u_crimson * 0.75 * max(lx * major, lz * majz);
    return c * fade;
}
"#;

/// full-screen backdrop: obsidian, floor grid, range rings, scanlines, vignette, and the
/// wireframe globe used by the greeter / lock screen (u_globe = radius scale, u_rot = spin).
pub const BACKDROP: &str = r#"
uniform float u_grid;
uniform float u_globe;
uniform float u_rot;
uniform float u_phase;
void main() {
    vec2 p = v_coords * size;
    vec3 c = u_bg;
    // horizon glow
    float hz = size.y * 0.58;
    c += u_amber * 0.05 * exp(-abs(p.y - hz) / (size.y * 0.03));
    if (u_grid > 0.5) c += floorgrid(p, size, 0.0) * u_phase;
    // faint range rings around the focal point
    vec2 q = (p - size * vec2(0.5, 0.48)) / size.y;
    float r = length(q);
    float ring = abs(fract(r * 6.0 - 0.5) - 0.5) / 6.0;
    c += u_amber * 0.035 * lineaa(ring * size.y, 1.0) * smoothstep(0.62, 0.1, r);
    // bearing ticks on the outer ring
    float ang = atan(q.y, q.x);
    float tick = abs(fract(ang / 6.2831853 * 72.0 + 0.5) - 0.5);
    c += u_amber * 0.10 * step(tick, 0.06) * lineaa(abs(r - 0.47) * size.y, 6.0);
    // globe
    if (u_globe > 0.001) {
        vec2 gc = size * vec2(0.5, 0.40);
        float R = size.y * 0.20 * u_globe;
        vec2 d = (p - gc) / R;
        float d2 = dot(d, d);
        if (d2 < 1.0) {
            vec3 n = vec3(d.x, -d.y, sqrt(1.0 - d2));
            float cr = cos(u_rot), sr = sin(u_rot);
            vec3 m = vec3(cr * n.x + sr * n.z, n.y, -sr * n.x + cr * n.z);
            float lat = asin(clamp(m.y, -1.0, 1.0));
            float lon = atan(m.x, m.z);
            float pxs = 1.0 / R;
            float glat = abs(fract(lat / 0.5236 + 0.5) - 0.5) * 0.5236;
            float glon = abs(fract(lon / 0.3927 + 0.5) - 0.5) * 0.3927 * max(cos(lat), 0.05);
            float wl = lineaa(glat / pxs, 1.2) + lineaa(glon / pxs, 1.2);
            float rim = smoothstep(0.80, 1.0, d2);
            c = mix(c, u_bg, 0.75);
            c += u_amber * (0.55 * clamp(wl, 0.0, 1.0) * (0.35 + 0.65 * n.z) + 0.25 * rim);
        }
        // orbit ellipse with a satellite
        vec2 e = (p - gc) / (R * vec2(1.55, 0.42));
        float el = abs(length(e) - 1.0) * R * 0.42;
        c += u_crimson * 0.45 * lineaa(el, 1.4);
        vec2 sat = gc + R * vec2(1.55 * cos(u_rot * 1.7), 0.42 * sin(u_rot * 1.7));
        c += u_crimson * smoothstep(5.0, 2.0, length(p - sat));
    }
    // scanlines + vignette
    c *= 0.94 + 0.06 * step(1.5, mod(p.y, 3.0));
    vec2 v = v_coords - 0.5;
    c *= 1.0 - 0.55 * dot(v, v);
    gl_FragColor = vec4(c, 1.0) * alpha;
}
"#;

/// centre stage: obsidian plane at plate_opacity, a softened copy of the floor grid seen
/// "through" it (the backdrop is procedural, so its blur is computed analytically instead of by
/// sampling), specular gloss along the top edge and a 1 px amber border.
pub const PLATE: &str = r#"
uniform float u_opacity;
uniform float u_blur;
uniform vec4 u_screen; // x, y of this element and w, h of the whole screen (physical px)
void main() {
    vec2 p = v_coords * size;
    vec3 c = u_bg;
    vec2 sp = u_screen.xy + p;
    if (u_blur > 0.5) c += floorgrid(sp, u_screen.zw, 1.0) * 0.6;
    float gloss = smoothstep(0.35, 0.0, v_coords.y) * smoothstep(1.0, 0.2, v_coords.x) * 0.06;
    c += vec3(gloss);
    float edge = min(min(p.x, size.x - p.x), min(p.y, size.y - p.y));
    float border = 1.0 - step(1.0, edge);
    vec4 o = vec4(c * u_opacity, u_opacity);
    o = mix(o, vec4(u_amber, 1.0), border);
    gl_FragColor = o * alpha;
}
"#;

/// ring node: wireframe bounding box with bright corner brackets and a faint depth fill.
pub const NODE: &str = r#"
uniform float u_opacity;
uniform float u_hl;
void main() {
    vec2 p = v_coords * size;
    float edge = min(min(p.x, size.x - p.x), min(p.y, size.y - p.y));
    float line = 1.0 - step(1.0, edge);
    vec2 q = min(p, size - p);
    float corner = (q.x < 14.0 && q.y < 14.0 && (q.x < 2.0 || q.y < 2.0)) ? 1.0 : 0.0;
    // obsidian fill so nearer nodes occlude the ones behind them, with a faint amber depth wash
    float fill = 0.84;
    vec3 fc = u_bg + u_amber * 0.05 * (1.0 - v_coords.y);
    float a = max(line * u_opacity, corner * min(1.0, u_opacity * 2.2));
    vec3 col = mix(u_amber, u_crimson, u_hl);
    vec4 o = mix(vec4(fc * fill, fill), vec4(col, 1.0), a);
    gl_FragColor = o * alpha;
}
"#;

/// HUD bar background (obsidian, amber rule on the inner edge)
pub const BAR: &str = r#"
uniform float u_top;
void main() {
    vec2 p = v_coords * size;
    float d = u_top > 0.5 ? size.y - p.y : p.y;
    vec4 o = vec4(u_bg * 0.92, 0.92);
    o = mix(o, vec4(u_amber * 0.8, 0.9), 1.0 - step(1.0, d));
    gl_FragColor = o * alpha;
}
"#;

pub struct Shaders {
    pub backdrop: GlesPixelProgram,
    pub plate: GlesPixelProgram,
    pub node: GlesPixelProgram,
    pub bar: GlesPixelProgram,
}

fn base<'a>() -> Vec<UniformName<'a>> {
    vec![UniformName::new("u_bg", UniformType::_3f), UniformName::new("u_amber", UniformType::_3f), UniformName::new("u_crimson", UniformType::_3f)]
}

fn with<'a>(extra: &[(&'a str, UniformType)]) -> Vec<UniformName<'a>> {
    let mut v = base();
    v.extend(extra.iter().map(|(n, t)| UniformName::new(*n, *t)));
    v
}

pub fn compile(r: &mut GlesRenderer) -> Result<Shaders, GlesError> {
    let src = |body: &str| format!("{COMMON}{body}");
    Ok(Shaders {
        backdrop: r.compile_custom_pixel_shader(src(BACKDROP), &with(&[("u_grid", UniformType::_1f), ("u_globe", UniformType::_1f), ("u_rot", UniformType::_1f), ("u_phase", UniformType::_1f)]))?,
        plate: r.compile_custom_pixel_shader(src(PLATE), &with(&[("u_opacity", UniformType::_1f), ("u_blur", UniformType::_1f), ("u_screen", UniformType::_4f)]))?,
        node: r.compile_custom_pixel_shader(src(NODE), &with(&[("u_opacity", UniformType::_1f), ("u_hl", UniformType::_1f)]))?,
        bar: r.compile_custom_pixel_shader(src(BAR), &with(&[("u_top", UniformType::_1f)]))?,
    })
}

