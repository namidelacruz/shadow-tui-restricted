//! Shadow's glyph icons — an original set drawn in thin gold line work: a carved cartouche (a
//! rounded block with a cheek band and three seed dots) around a motif per app or action. No
//! dependencies, so the compositor includes this file directly (#[path]) and draws the same glyphs
//! as the apps.
//!
//! Glyphs are lists of primitives in a 0..1 box, rasterised with analytic anti-aliasing into a
//! coverage mask (0..1 per pixel) that the caller tints.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum G {
    Terminal,
    Files,
    Browser,
    Launch,
    Tasks,
    Craft,
    Settings,
    Fix,
    Network,
    Power,
    Lock,
    Logout,
    Restart,
    Suspend,
    Music,
    Image,
    Code,
    Chat,
    Mail,
    Game,
    Generic,
    Emergency,
    Clock,
    System,
    Battery,
    Volume,
    Brightness,
    Desktop,
    Update,
    User,
}

pub const ALL: &[G] = &[
    G::Terminal, G::Files, G::Browser, G::Launch, G::Tasks, G::Craft, G::Settings, G::Fix, G::Network, G::Power, G::Lock, G::Logout, G::Restart,
    G::Suspend, G::Music, G::Image, G::Code, G::Chat, G::Mail, G::Game, G::Generic, G::Emergency, G::Clock, G::System, G::Battery, G::Volume,
    G::Brightness, G::Desktop, G::Update, G::User,
];

#[derive(Clone, Copy, Debug)]
pub enum P {
    /// segment
    L(f32, f32, f32, f32),
    /// arc: centre, radius, start and end angle in turns (0 = east, 0.25 = south)
    A(f32, f32, f32, f32, f32),
    /// ring
    C(f32, f32, f32),
    /// filled dot
    D(f32, f32, f32),
    /// rounded rectangle outline: x, y, w, h, corner radius
    R(f32, f32, f32, f32, f32),
}

/// The glyph for a window's app id (or a desktop file name).
pub fn for_app(app_id: &str) -> G {
    let a = app_id.to_ascii_lowercase();
    let has = |k: &[&str]| k.iter().any(|w| a.contains(w));
    if has(&["term", "foot", "kitty", "alacritty", "wezterm", "konsole", "ghostty", "xterm", "ptyxis"]) {
        G::Terminal
    } else if has(&["files", "nautilus", "dolphin", "thunar", "nemo", "yazi", "pcmanfm"]) {
        G::Files
    } else if has(&["firefox", "chrom", "brave", "vivaldi", "librewolf", "zen", "browser", "epiphany", "edge"]) {
        G::Browser
    } else if has(&["launch"]) {
        G::Launch
    } else if has(&["tasks", "monitor", "btop", "htop"]) {
        G::Tasks
    } else if has(&["craft"]) {
        G::Craft
    } else if has(&["config", "settings", "prefs", "control"]) {
        G::Settings
    } else if has(&["fix", "passfix", "repair"]) {
        G::Fix
    } else if has(&["net", "wifi", "nm-"]) {
        G::Network
    } else if has(&["music", "spotify", "rhythm", "audio", "player", "mpv", "vlc", "celluloid"]) {
        G::Music
    } else if has(&["image", "photo", "gimp", "krita", "inkscape", "loupe", "eog", "darktable"]) {
        G::Image
    } else if has(&["code", "codium", "zed", "nvim", "vim", "emacs", "helix", "jetbrains", "idea", "kate", "gedit", "text"]) {
        G::Code
    } else if has(&["discord", "vesktop", "slack", "signal", "telegram", "element", "chat", "teams"]) {
        G::Chat
    } else if has(&["mail", "thunderbird", "evolution", "geary"]) {
        G::Mail
    } else if has(&["steam", "lutris", "heroic", "game", "minecraft"]) {
        G::Game
    } else if has(&["update"]) {
        G::Update
    } else {
        G::Generic
    }
}

/// 10.0 bilingual labels: the Japanese word shown beside the English one (設定 SETTINGS).
pub fn jp(g: G) -> &'static str {
    match g {
        G::Terminal => "端末",
        G::Files => "ファイル",
        G::Browser => "ブラウザ",
        G::Launch => "起動",
        G::Tasks => "タスク",
        G::Craft => "工房",
        G::Settings => "設定",
        G::Fix => "修復",
        G::Network => "通信",
        G::Power => "電源",
        G::Lock => "施錠",
        G::Logout => "退出",
        G::Restart => "再起動",
        G::Suspend => "休止",
        G::Music => "音楽",
        G::Image => "画像",
        G::Code => "コード",
        G::Chat => "会話",
        G::Mail => "メール",
        G::Game => "遊戯",
        G::Generic => "アプリ",
        G::Emergency => "緊急",
        G::Clock => "時計",
        G::System => "系統",
        G::Battery => "電池",
        G::Volume => "音量",
        G::Brightness => "輝度",
        G::Desktop => "机上",
        G::Update => "更新",
        G::User => "利用者",
    }
}

/// the English label of a glyph (what the bilingual label pairs with the Japanese one)
pub fn en(g: G) -> &'static str {
    match g {
        G::Terminal => "TERMINAL",
        G::Files => "FILES",
        G::Browser => "BROWSER",
        G::Launch => "LAUNCH",
        G::Tasks => "TASKS",
        G::Craft => "CRAFT",
        G::Settings => "SETTINGS",
        G::Fix => "FIX",
        G::Network => "NETWORK",
        G::Power => "POWER",
        G::Lock => "LOCK",
        G::Logout => "LOG OUT",
        G::Restart => "RESTART",
        G::Suspend => "SLEEP",
        G::Music => "MUSIC",
        G::Image => "IMAGES",
        G::Code => "CODE",
        G::Chat => "CHAT",
        G::Mail => "MAIL",
        G::Game => "GAMES",
        G::Generic => "APP",
        G::Emergency => "EMERGENCY",
        G::Clock => "CLOCK",
        G::System => "SYSTEM",
        G::Battery => "BATTERY",
        G::Volume => "VOLUME",
        G::Brightness => "BRIGHTNESS",
        G::Desktop => "DESKTOP",
        G::Update => "UPDATE",
        G::User => "USER",
    }
}

/// "設定 SETTINGS"
pub fn bilingual(g: G) -> String {
    format!("{} {}", jp(g), en(g))
}

/// the carved block every app glyph sits in
fn cartouche(v: &mut Vec<P>) {
    v.push(P::R(0.08, 0.10, 0.84, 0.80, 0.20));
    // cheek band on the left, as on carved blocks: a groove with a curl at the top
    v.push(P::L(0.30, 0.30, 0.30, 0.80));
    v.push(P::A(0.24, 0.30, 0.06, 0.50, 1.0));
    // seed dots
    v.push(P::D(0.19, 0.46, 0.022));
    v.push(P::D(0.19, 0.58, 0.022));
    v.push(P::D(0.19, 0.70, 0.022));
}

pub fn prims(g: G) -> Vec<P> {
    use self::P::*;
    let mut v = vec![];
    let framed = !matches!(g, G::Power | G::Restart | G::Suspend | G::Logout | G::Lock | G::Emergency | G::Clock | G::Volume | G::Brightness | G::Battery | G::User);
    if framed {
        cartouche(&mut v);
    }
    // motif area: x 0.36..0.86, y 0.22..0.78 when framed
    match g {
        G::Terminal => v.extend([L(0.42, 0.34, 0.58, 0.48), L(0.58, 0.48, 0.42, 0.62), L(0.62, 0.66, 0.80, 0.66)]),
        G::Files => v.extend([R(0.40, 0.30, 0.42, 0.40, 0.05), L(0.40, 0.38, 0.82, 0.38), L(0.44, 0.76, 0.86, 0.76), L(0.86, 0.36, 0.86, 0.76)]),
        G::Browser => v.extend([C(0.61, 0.50, 0.22), A(0.61, 0.50, 0.11, 0.75, 1.25), L(0.39, 0.50, 0.83, 0.50), A(0.61, 0.28, 0.30, 0.17, 0.33)]),
        G::Launch => v.extend([L(0.61, 0.24, 0.61, 0.76), L(0.37, 0.50, 0.85, 0.50), L(0.53, 0.42, 0.69, 0.58), L(0.69, 0.42, 0.53, 0.58), D(0.61, 0.50, 0.04)]),
        G::Tasks => v.extend([L(0.44, 0.70, 0.44, 0.50), L(0.56, 0.70, 0.56, 0.32), L(0.68, 0.70, 0.68, 0.44), L(0.80, 0.70, 0.80, 0.58), L(0.40, 0.74, 0.84, 0.74)]),
        G::Craft => v.extend([L(0.61, 0.24, 0.84, 0.50), L(0.84, 0.50, 0.61, 0.76), L(0.61, 0.76, 0.38, 0.50), L(0.38, 0.50, 0.61, 0.24), L(0.61, 0.38, 0.72, 0.50), L(0.72, 0.50, 0.61, 0.62), L(0.61, 0.62, 0.50, 0.50), L(0.50, 0.50, 0.61, 0.38)]),
        G::Settings => {
            v.push(C(0.61, 0.50, 0.11));
            for i in 0..8 {
                let a = i as f32 / 8.0 * std::f32::consts::TAU;
                v.push(L(0.61 + 0.16 * a.cos(), 0.50 + 0.16 * a.sin(), 0.61 + 0.23 * a.cos(), 0.50 + 0.23 * a.sin()));
            }
        }
        G::Fix => v.extend([L(0.42, 0.30, 0.80, 0.70), L(0.80, 0.30, 0.42, 0.70), C(0.42, 0.30, 0.05), C(0.80, 0.30, 0.05), D(0.61, 0.50, 0.035)]),
        G::Network => v.extend([A(0.61, 0.70, 0.36, 0.60, 0.90), A(0.61, 0.70, 0.24, 0.60, 0.90), A(0.61, 0.70, 0.12, 0.60, 0.90), D(0.61, 0.70, 0.035)]),
        G::Music => v.extend([C(0.50, 0.68, 0.07), L(0.57, 0.68, 0.57, 0.28), L(0.57, 0.28, 0.80, 0.36), L(0.80, 0.36, 0.80, 0.60), C(0.73, 0.60, 0.07)]),
        G::Image => v.extend([L(0.38, 0.72, 0.54, 0.46), L(0.54, 0.46, 0.66, 0.62), L(0.66, 0.62, 0.72, 0.54), L(0.72, 0.54, 0.86, 0.72), L(0.38, 0.72, 0.86, 0.72), C(0.74, 0.34, 0.06)]),
        G::Code => v.extend([L(0.50, 0.32, 0.38, 0.50), L(0.38, 0.50, 0.50, 0.68), L(0.72, 0.32, 0.84, 0.50), L(0.84, 0.50, 0.72, 0.68), L(0.66, 0.30, 0.56, 0.70)]),
        G::Chat => v.extend([R(0.38, 0.28, 0.48, 0.34, 0.10), L(0.48, 0.62, 0.44, 0.74), L(0.44, 0.74, 0.58, 0.62), D(0.52, 0.45, 0.03), D(0.62, 0.45, 0.03), D(0.72, 0.45, 0.03)]),
        G::Mail => v.extend([R(0.38, 0.32, 0.48, 0.36, 0.04), L(0.38, 0.34, 0.62, 0.54), L(0.62, 0.54, 0.86, 0.34)]),
        G::Game => v.extend([R(0.36, 0.36, 0.50, 0.28, 0.12), L(0.46, 0.50, 0.56, 0.50), L(0.51, 0.45, 0.51, 0.55), D(0.72, 0.46, 0.03), D(0.78, 0.54, 0.03)]),
        G::Generic => v.extend([A(0.61, 0.50, 0.20, 0.0, 0.75), A(0.61, 0.45, 0.13, 0.75, 1.5), A(0.61, 0.50, 0.06, 0.5, 1.25)]),
        G::Desktop => v.extend([R(0.38, 0.30, 0.48, 0.32, 0.04), L(0.62, 0.62, 0.62, 0.72), L(0.50, 0.72, 0.74, 0.72)]),
        G::Update => v.extend([L(0.61, 0.28, 0.61, 0.66), L(0.61, 0.66, 0.50, 0.55), L(0.61, 0.66, 0.72, 0.55), L(0.42, 0.74, 0.80, 0.74)]),
        G::System => {
            v.push(R(0.46, 0.36, 0.30, 0.28, 0.03));
            for i in 0..3 {
                let t = 0.42 + i as f32 * 0.08;
                v.extend([L(t, 0.28, t, 0.36), L(t, 0.64, t, 0.72)]);
            }
            v.extend([L(0.38, 0.46, 0.46, 0.46), L(0.38, 0.54, 0.46, 0.54), L(0.76, 0.46, 0.84, 0.46), L(0.76, 0.54, 0.84, 0.54)]);
        }
        // unframed: actions and status glyphs (a carved ring instead of the block)
        G::Power => v.extend([A(0.50, 0.52, 0.28, 0.83, 1.67), L(0.50, 0.16, 0.50, 0.50)]),
        G::Restart => v.extend([A(0.50, 0.52, 0.28, 0.90, 1.70), L(0.50, 0.24, 0.62, 0.16), L(0.50, 0.24, 0.60, 0.33)]),
        G::Suspend => v.extend([A(0.50, 0.50, 0.30, 0.20, 0.80), A(0.62, 0.42, 0.24, 0.30, 0.70)]),
        G::Logout => v.extend([L(0.40, 0.18, 0.22, 0.18), L(0.22, 0.18, 0.22, 0.82), L(0.22, 0.82, 0.40, 0.82), L(0.42, 0.50, 0.84, 0.50), L(0.84, 0.50, 0.70, 0.38), L(0.84, 0.50, 0.70, 0.62)]),
        G::Lock => v.extend([R(0.24, 0.46, 0.52, 0.38, 0.06), A(0.50, 0.46, 0.17, 0.50, 1.0), L(0.33, 0.46, 0.33, 0.38), L(0.67, 0.46, 0.67, 0.38), D(0.50, 0.64, 0.04)]),
        G::Emergency => v.extend([L(0.50, 0.14, 0.88, 0.82), L(0.88, 0.82, 0.12, 0.82), L(0.12, 0.82, 0.50, 0.14), L(0.50, 0.38, 0.50, 0.60), D(0.50, 0.71, 0.035)]),
        G::Clock => v.extend([C(0.50, 0.50, 0.34), L(0.50, 0.50, 0.50, 0.28), L(0.50, 0.50, 0.66, 0.58), D(0.50, 0.50, 0.03)]),
        G::Battery => v.extend([R(0.14, 0.32, 0.64, 0.36, 0.06), L(0.82, 0.44, 0.82, 0.56), L(0.24, 0.50, 0.56, 0.50)]),
        G::Volume => v.extend([L(0.16, 0.42, 0.28, 0.42), L(0.28, 0.42, 0.44, 0.28), L(0.44, 0.28, 0.44, 0.72), L(0.44, 0.72, 0.28, 0.58), L(0.28, 0.58, 0.16, 0.58), L(0.16, 0.58, 0.16, 0.42), A(0.46, 0.50, 0.18, 0.88, 1.12), A(0.46, 0.50, 0.32, 0.88, 1.12)]),
        G::Brightness => {
            v.push(C(0.50, 0.50, 0.14));
            for i in 0..8 {
                let a = i as f32 / 8.0 * std::f32::consts::TAU;
                v.push(L(0.50 + 0.22 * a.cos(), 0.50 + 0.22 * a.sin(), 0.50 + 0.32 * a.cos(), 0.50 + 0.32 * a.sin()));
            }
        }
        G::User => v.extend([C(0.50, 0.36, 0.14), A(0.50, 0.86, 0.32, 0.55, 0.95)]),
    }
    v
}

fn seg(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let t = (((px - x0) * dx + (py - y0) * dy) / (dx * dx + dy * dy).max(1e-9)).clamp(0.0, 1.0);
    ((px - x0 - t * dx).powi(2) + (py - y0 - t * dy).powi(2)).sqrt()
}

/// distance from a point to a primitive's line (0 inside a dot)
fn dist(p: &P, x: f32, y: f32) -> f32 {
    match *p {
        P::L(x0, y0, x1, y1) => seg(x, y, x0, y0, x1, y1),
        P::C(cx, cy, r) => (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r).abs(),
        P::D(cx, cy, r) => (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r).max(0.0),
        P::A(cx, cy, r, a0, a1) => {
            let ang = (y - cy).atan2(x - cx) / std::f32::consts::TAU;
            let inside = |a: f32| {
                let mut t = a;
                while t < a0 {
                    t += 1.0;
                }
                t <= a1
            };
            if inside(ang) {
                (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r).abs()
            } else {
                let e = |a: f32| {
                    let t = a * std::f32::consts::TAU;
                    ((x - cx - r * t.cos()).powi(2) + (y - cy - r * t.sin()).powi(2)).sqrt()
                };
                e(a0).min(e(a1))
            }
        }
        P::R(rx, ry, w, h, r) => {
            let (cx, cy) = (rx + w / 2.0, ry + h / 2.0);
            let (qx, qy) = ((x - cx).abs() - (w / 2.0 - r), (y - cy).abs() - (h / 2.0 - r));
            let out = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
            out.abs()
        }
    }
}

/// The glyph as SVG (for the web shell): strokes in `currentColor`, viewBox 0 0 100 100.
pub fn svg(g: G) -> String {
    let k = |v: f32| (v * 100.0 * 100.0).round() / 100.0;
    let mut body = String::new();
    for p in prims(g) {
        match p {
            P::L(x0, y0, x1, y1) => body += &format!("<line x1='{}' y1='{}' x2='{}' y2='{}'/>", k(x0), k(y0), k(x1), k(y1)),
            P::A(cx, cy, r, a0, a1) => {
                let pt = |t: f32| (cx + r * (t * std::f32::consts::TAU).cos(), cy + r * (t * std::f32::consts::TAU).sin());
                let (s0, e0) = (pt(a0), pt(a1));
                let large = if (a1 - a0).abs() > 0.5 { 1 } else { 0 };
                let sweep = if a1 > a0 { 1 } else { 0 };
                if (a1 - a0).abs() >= 0.999 {
                    body += &format!("<circle cx='{}' cy='{}' r='{}'/>", k(cx), k(cy), k(r));
                } else {
                    body += &format!("<path d='M{} {}A{} {} 0 {large} {sweep} {} {}'/>", k(s0.0), k(s0.1), k(r), k(r), k(e0.0), k(e0.1));
                }
            }
            P::C(cx, cy, r) => body += &format!("<circle cx='{}' cy='{}' r='{}'/>", k(cx), k(cy), k(r)),
            P::D(cx, cy, r) => body += &format!("<circle cx='{}' cy='{}' r='{}' fill='currentColor' stroke='none'/>", k(cx), k(cy), k(r)),
            P::R(x, y, w, h, r) => body += &format!("<rect x='{}' y='{}' width='{}' height='{}' rx='{}'/>", k(x), k(y), k(w), k(h), k(r)),
        }
    }
    format!("<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100' fill='none' stroke='currentColor' stroke-width='4.6' stroke-linecap='round' stroke-linejoin='round'>{body}</svg>")
}

/// a glyph by its lower-case name ("terminal", "settings", …)
pub fn by_name(name: &str) -> Option<G> {
    ALL.iter().copied().find(|g| format!("{g:?}").eq_ignore_ascii_case(name))
}

/// Coverage mask, `px` × `px`, row-major. `stroke` is the line width in pixels (≥ 1).
pub fn raster(g: G, px: usize, stroke: f32) -> Vec<f32> {
    let prims = prims(g);
    let mut out = vec![0f32; px * px];
    let s = px as f32;
    let half = stroke.max(1.0) / 2.0;
    for y in 0..px {
        for x in 0..px {
            let (fx, fy) = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
            let d = prims.iter().map(|p| dist(p, fx, fy)).fold(f32::MAX, f32::min) * s;
            // analytic anti-aliasing: a one-pixel ramp at the line's edge
            out[y * px + x] = (half + 0.5 - d).clamp(0.0, 1.0);
        }
    }
    out
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn every_glyph_has_ink_and_stays_inside() {
        for g in ALL {
            let m = raster(*g, 48, 2.0);
            let ink = m.iter().filter(|v| **v > 0.5).count();
            assert!(ink > 40, "{g:?} is nearly empty ({ink})");
            assert!(ink < 48 * 48 / 2, "{g:?} is a blob ({ink})");
            // a one-pixel border stays clear: nothing is clipped at the edge
            for i in 0..48 {
                for (x, y) in [(i, 0), (0, i), (i, 47), (47, i)] {
                    assert!(m[y * 48 + x] < 0.5, "{g:?} touches the edge at {x},{y}");
                }
            }
        }
    }
    #[test]
    fn apps_get_their_glyphs() {
        assert_eq!(for_app("foot"), G::Terminal);
        assert_eq!(for_app("org.mozilla.firefox"), G::Browser);
        assert_eq!(for_app("shadow-files"), G::Files);
        assert_eq!(for_app("com.discordapp.Discord"), G::Chat);
        assert_eq!(for_app("something.else"), G::Generic);
    }
}
