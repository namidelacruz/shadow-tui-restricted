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


/// The 9.0 background: warm obsidian with a stone grain, faint contour lines (iso-lines of a
/// noise field), five brighter trail lines the light travels along (trails() — the same curves
/// as glint.rs), and the hidden hub's star: rays and dashed rings around u_star, radius u_starr.
/// Still: no time uniform, so it costs nothing until something above it changes.
/// u_reveal (0..1) draws the lines in from the centre (login boot animation).
pub const BACKGROUND: &str = r#"
uniform vec3 u_hi;
uniform float u_seed;
uniform vec2 u_star;
uniform float u_starr;
uniform float u_reveal;
float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }
float noise(vec2 p) {
    vec2 i = floor(p); vec2 f = fract(p); vec2 u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x), mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}
float fbm(vec2 p) {
    float v = 0.0; float a = 0.5;
    for (int i = 0; i < 4; i++) { v += a * noise(p); p = p * 2.03 + vec2(1.7, 9.2); a *= 0.5; }
    return v;
}

// the five trail lines, in fractions of the screen: y(u) = c + a sin(2π(f u + p)) + b sin(2π(g u + q))
vec4 trailA(int i) {
    if (i == 0) return vec4(0.16, 0.050, 0.70, 0.10);
    if (i == 1) return vec4(0.34, 0.070, 0.45, 0.60);
    if (i == 2) return vec4(0.53, 0.045, 0.90, 0.30);
    if (i == 3) return vec4(0.71, 0.065, 0.55, 0.85);
    return vec4(0.88, 0.040, 0.80, 0.45);
}
vec3 trailB(int i) {
    if (i == 0) return vec3(0.022, 1.9, 0.4);
    if (i == 1) return vec3(0.028, 2.3, 0.2);
    if (i == 2) return vec3(0.035, 1.6, 0.7);
    if (i == 3) return vec3(0.020, 2.8, 0.1);
    return vec3(0.026, 2.1, 0.9);
}
float trailY(int i, float u) { vec4 A = trailA(i); vec3 B = trailB(i); return A.x + A.y * sin(6.28318 * (A.z * u + A.w)) + B.x * sin(6.28318 * (B.y * u + B.z)); }
float trailDY(int i, float u) { vec4 A = trailA(i); vec3 B = trailB(i); return 6.28318 * (A.y * A.z * cos(6.28318 * (A.z * u + A.w)) + B.x * B.y * cos(6.28318 * (B.y * u + B.z))); }

void main() {
    vec2 p = v_coords * size;
    float H = size.y;
    vec3 c = u_bg;
    // stone grain and a warm wash
    float grain = fbm(p / H * 7.0 + u_seed * 13.0);
    c += u_amber * 0.045 * (grain - 0.45);
    c += vec3(0.012) * (hash(floor(p * 0.5) + u_seed) - 0.5);
    // reveal: lines draw in from the centre of the screen
    float dc = length(p - size * 0.5) / length(size * 0.5);
    float rev = smoothstep(u_reveal * 1.15, u_reveal * 1.15 - 0.12, dc);
    // contour lines
    vec2 q = p / H * 1.7 + vec2(u_seed * 3.1, u_seed * 1.7);
    float e = 1.7 / H;
    float n = fbm(q);
    vec2 gr = vec2(fbm(q + vec2(e, 0.0)) - n, fbm(q + vec2(0.0, e)) - n);
    float val = n * 16.0;
    float d = abs(fract(val + 0.5) - 0.5) / max(length(gr) * 16.0, 1e-4);
    float major = mod(floor(val + 0.5), 4.0) == 0.0 ? 1.0 : 0.0;
    c += u_amber * lineaa(d, 0.9) * (0.055 + 0.075 * major) * rev;
    // trail lines (where the light travels)
    for (int i = 0; i < 5; i++) {
        float u = p.x / size.x;
        float y = H * trailY(i, u);
        float slope = H / size.x * trailDY(i, u);
        float dd = abs(p.y - y) / sqrt(1.0 + slope * slope);
        c += u_amber * 0.20 * lineaa(dd, 1.1) * rev;
        // fine fibre beside each trail
        c += u_amber * 0.05 * lineaa(abs(dd - 7.0), 0.7) * rev;
    }
    // the hidden hub: a Vitruvian figure — a circle and a square sharing their base, the
    // square graduated in eighths like a canon of proportions, eight faint rays, a soft core
    if (u_starr > 0.0) {
        vec2 v = p - u_star; float r = length(v);
        float R = u_starr;
        float circ = lineaa(abs(r - R), 1.0);
        float S = R * 1.64;
        vec2 sq = vec2(u_star.x, u_star.y + R - S * 0.5);   // square centre: bases aligned
        vec2 qv = abs(p - sq) - vec2(S * 0.5);
        float sd = abs(max(qv.x, qv.y));
        float square = lineaa(sd, 1.0) * step(max(qv.x, qv.y), 1.5);
        // graduation ticks along the square's sides, inward
        vec2 lp = (p - (sq - vec2(S * 0.5))) / S * 8.0;
        float tx = lineaa(abs(fract(lp.x + 0.5) - 0.5) * S / 8.0, 0.9) * step(abs(qv.y), 9.0) * step(qv.x, 0.0);
        float ty = lineaa(abs(fract(lp.y + 0.5) - 0.5) * S / 8.0, 0.9) * step(abs(qv.x), 9.0) * step(qv.y, 0.0);
        float ang = atan(v.y, v.x);
        float k = 8.0;
        float ad = abs(fract(ang / 6.28318 * k + 0.5) - 0.5) * 6.28318 / k * r;
        float ray = lineaa(ad, 0.8) * smoothstep(R, R * 0.2, r) * smoothstep(14.0, 60.0, r);
        float inner = lineaa(abs(r - R * 0.5), 0.8) * step(0.55, fract(ang / 6.28318 * 64.0));
        c += u_amber * (0.16 * circ + 0.14 * square + 0.10 * (tx + ty) + 0.06 * ray + 0.07 * inner) * rev;
        c += u_hi * 0.28 * exp(-r / (R * 0.03)) * rev;
    }
    // vignette
    c *= mix(1.0, 0.62, smoothstep(0.45, 1.15, length((v_coords - 0.5) * vec2(1.25, 1.0)) * 1.4));
    gl_FragColor = vec4(c, 1.0) * alpha;
}
"#;

/// One travelling light on trail `u_trail`: a bright point at u_head (0..1 across the screen)
/// with a fading tail behind it along the line. Drawn as a small quad that moves, so only the
/// quad's old and new places are redrawn. u_screen = (quad x, quad y, screen w, screen h) px.
pub const GLINT: &str = r#"
uniform vec3 u_hi;
uniform float u_trail;
uniform float u_head;
uniform float u_tail;
uniform vec4 u_screen;

// the five trail lines, in fractions of the screen: y(u) = c + a sin(2π(f u + p)) + b sin(2π(g u + q))
vec4 trailA(int i) {
    if (i == 0) return vec4(0.16, 0.050, 0.70, 0.10);
    if (i == 1) return vec4(0.34, 0.070, 0.45, 0.60);
    if (i == 2) return vec4(0.53, 0.045, 0.90, 0.30);
    if (i == 3) return vec4(0.71, 0.065, 0.55, 0.85);
    return vec4(0.88, 0.040, 0.80, 0.45);
}
vec3 trailB(int i) {
    if (i == 0) return vec3(0.022, 1.9, 0.4);
    if (i == 1) return vec3(0.028, 2.3, 0.2);
    if (i == 2) return vec3(0.035, 1.6, 0.7);
    if (i == 3) return vec3(0.020, 2.8, 0.1);
    return vec3(0.026, 2.1, 0.9);
}
float trailY(int i, float u) { vec4 A = trailA(i); vec3 B = trailB(i); return A.x + A.y * sin(6.28318 * (A.z * u + A.w)) + B.x * sin(6.28318 * (B.y * u + B.z)); }
float trailDY(int i, float u) { vec4 A = trailA(i); vec3 B = trailB(i); return 6.28318 * (A.y * A.z * cos(6.28318 * (A.z * u + A.w)) + B.x * B.y * cos(6.28318 * (B.y * u + B.z))); }

void main() {
    vec2 p = u_screen.xy + v_coords * size;
    vec2 S = u_screen.zw;
    int ti = int(u_trail + 0.5);
    float u = p.x / S.x;
    float y = S.y * trailY(ti, u);
    float slope = S.y / S.x * trailDY(ti, u);
    float dd = abs(p.y - y) / sqrt(1.0 + slope * slope);
    float behind = u_head - u;
    float tail = behind > 0.0 ? pow(clamp(1.0 - behind / u_tail, 0.0, 1.0), 2.2) : 0.0;
    float line = (1.0 - smoothstep(0.6, 2.2, dd)) * tail * 0.75;
    vec2 hp = vec2(u_head * S.x, S.y * trailY(ti, u_head));
    float r = length(p - hp);
    float glow = exp(-r / 3.0) * 1.2 + exp(-r / 16.0) * 0.35;
    // a four-point sparkle on the head
    vec2 dv = abs(p - hp);
    float spark = (exp(-dv.y / 1.2) * exp(-dv.x / 18.0) + exp(-dv.x / 1.2) * exp(-dv.y / 18.0)) * 0.5;
    float i = clamp(line + glow + spark, 0.0, 1.6);
    vec3 col = mix(u_amber, u_hi, clamp(glow + spark, 0.0, 1.0));
    float a = clamp(i, 0.0, 1.0) * alpha;
    gl_FragColor = vec4(col * a, a);
}
"#;

/// The frame around an app tile or a minimised square: a thin gold line with brighter corner
/// marks; u_focus (0..1) brightens it and adds a notch mark at the top, u_fill fills it with
/// obsidian (for minimised tiles), u_hover lifts it under the pointer.
pub const FRAME: &str = r#"
uniform vec3 u_hi;
uniform float u_focus;
uniform float u_fill;
uniform float u_hover;
void main() {
    vec2 p = v_coords * size;
    vec2 s = size;
    float d = min(min(p.x, s.x - p.x), min(p.y, s.y - p.y));
    float edge = lineaa(abs(d - 1.5), 1.0);
    // corner marks
    float m = min(s.x, s.y) * 0.12 + 6.0;
    vec2 cp = min(p, s - p);
    float corner = (cp.x < m && cp.y < m) ? lineaa(min(abs(cp.x - 4.0), abs(cp.y - 4.0)), 1.6) * step(min(cp.x, cp.y), m) : 0.0;
    // top notch (focused): a small diamond on the top edge
    vec2 nv = abs(p - vec2(s.x * 0.5, 1.5));
    float notch = u_focus * (1.0 - smoothstep(5.0, 6.5, nv.x + nv.y));
    float glow = u_focus * 0.18 * exp(-d / 10.0);
    // ruler graduations along the top and left edges (every 10 px, longer every 50)
    float gx = mod(p.x, 10.0); float gy = mod(p.y, 10.0);
    float longx = mod(p.x, 50.0) < 1.2 ? 7.0 : 3.5;
    float longy = mod(p.y, 50.0) < 1.2 ? 7.0 : 3.5;
    float ruler = (step(gx, 1.0) * step(p.y, longx + 2.0) * step(m, p.x) * step(m, s.x - p.x)
                 + step(gy, 1.0) * step(p.x, longy + 2.0) * step(m, p.y) * step(m, s.y - p.y)) * (0.30 + 0.35 * u_focus);
    corner += ruler;
    float lvl = 0.38 + 0.42 * u_focus + 0.25 * u_hover;
    vec3 col = u_amber * (edge * lvl + glow) + u_hi * (corner * (0.45 + 0.5 * u_focus) + notch);
    float a = clamp(edge * lvl + corner * 0.9 + notch + glow, 0.0, 1.0);
    vec4 frame = vec4(col, a);
    vec4 fill = vec4(u_bg * 0.92, 1.0) * u_fill * 0.88 * step(2.5, d);
    // frame over fill
    vec4 o = frame + fill * (1.0 - frame.a);
    gl_FragColor = o * alpha;
}
"#;

/// The tessellation drawn over a window while it unfolds: the cell grid, a diagonal in every cell
/// (alternating, so the cells read as triangle pairs), and a circle squared into the rect. The
/// lines are brightest at the wave front (distance from u_origin ≈ u_front).
pub const TESS: &str = r#"
uniform vec3 u_hi;
uniform vec2 u_cells;
uniform vec2 u_origin;
uniform float u_front;
uniform float u_lines;
void main() {
    vec2 p = v_coords * size;
    vec2 cs = size / u_cells;
    vec2 g = p / cs;
    vec2 f = fract(g);
    vec2 cell = floor(g);
    // grid lines
    vec2 dl = min(f, 1.0 - f) * cs;
    float grid = lineaa(min(dl.x, dl.y), 0.9);
    // diagonals, alternating per cell
    float flip = mod(cell.x + cell.y, 2.0);
    vec2 fd = flip > 0.5 ? vec2(1.0 - f.x, f.y) : f;
    float diag = lineaa(abs(fd.x - fd.y) * min(cs.x, cs.y) * 0.7071, 0.8);
    // the circle squared into the window
    vec2 cc = size * 0.5;
    float R = min(size.x, size.y) * 0.5 - 2.0;
    float circ = lineaa(abs(length(p - cc) - R), 1.0);
    // brightness follows the wave front
    float d = length(v_coords - u_origin) / 1.4142;
    float front = exp(-pow((d - u_front) / 0.12, 2.0));
    float i = (grid * 0.55 + diag * 0.30 + circ * 0.45) * (0.35 + 0.65 * front) * u_lines;
    vec3 col = mix(u_amber, u_hi, front);
    float a = clamp(i, 0.0, 1.0) * alpha;
    gl_FragColor = vec4(col * a, a);
}
"#;

pub struct Shaders {
    pub backdrop: GlesPixelProgram,
    pub plate: GlesPixelProgram,
    pub node: GlesPixelProgram,
    pub bar: GlesPixelProgram,
    pub background: GlesPixelProgram,
    pub glint: GlesPixelProgram,
    pub frame: GlesPixelProgram,
    pub tess: GlesPixelProgram,
}

/// The trail lines on the CPU, the same constants as the shaders: (c, a, f, p, b, g, q)
pub const TRAILS: [[f64; 7]; 5] = [
    [0.16, 0.050, 0.70, 0.10, 0.022, 1.9, 0.4],
    [0.34, 0.070, 0.45, 0.60, 0.028, 2.3, 0.2],
    [0.53, 0.045, 0.90, 0.30, 0.035, 1.6, 0.7],
    [0.71, 0.065, 0.55, 0.85, 0.020, 2.8, 0.1],
    [0.88, 0.040, 0.80, 0.45, 0.026, 2.1, 0.9],
];
pub fn trail_y(i: usize, u: f64) -> f64 {
    let t = TRAILS[i];
    let tau = std::f64::consts::TAU;
    t[0] + t[1] * (tau * (t[2] * u + t[3])).sin() + t[4] * (tau * (t[5] * u + t[6])).sin()
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
        background: r.compile_custom_pixel_shader(src(BACKGROUND), &with(&[("u_hi", UniformType::_3f), ("u_seed", UniformType::_1f), ("u_star", UniformType::_2f), ("u_starr", UniformType::_1f), ("u_reveal", UniformType::_1f)]))?,
        glint: r.compile_custom_pixel_shader(src(GLINT), &with(&[("u_hi", UniformType::_3f), ("u_trail", UniformType::_1f), ("u_head", UniformType::_1f), ("u_tail", UniformType::_1f), ("u_screen", UniformType::_4f)]))?,
        tess: r.compile_custom_pixel_shader(src(TESS), &with(&[("u_hi", UniformType::_3f), ("u_cells", UniformType::_2f), ("u_origin", UniformType::_2f), ("u_front", UniformType::_1f), ("u_lines", UniformType::_1f)]))?,
        frame: r.compile_custom_pixel_shader(src(FRAME), &with(&[("u_hi", UniformType::_3f), ("u_focus", UniformType::_1f), ("u_fill", UniformType::_1f), ("u_hover", UniformType::_1f)]))?,
    })
}

