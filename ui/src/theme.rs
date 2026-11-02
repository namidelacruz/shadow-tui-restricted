//! Tactical Amber/Cyan theme shared by every app; colours and font come from
//! ~/.config/shadow-state/config.toml [look] when set.
use crate::canvas::Rgb;
use std::path::PathBuf;

#[derive(Clone)]
pub struct Theme {
    pub bg: Rgb,
    pub bg_alpha: f32,
    pub panel: Rgb,
    pub amber: Rgb,
    pub cyan: Rgb,
    pub crimson: Rgb,
    pub green: Rgb,
    pub text: Rgb,
    pub dim: Rgb,
    /// secondary accent for gradients (amber → violet → cyan)
    pub violet: Rgb,
    /// glass panels: top / bottom of their gradient
    pub glass: Rgb,
    pub glass_lo: Rgb,
    /// corner radius in logical px ([look] radius)
    pub radius: f32,
    pub font: String,
    /// body text size in points (96 dpi reference, before scaling)
    pub font_pt: f32,
    /// opacity of app windows (glass); 1.0 = fully opaque ([look] window_opacity)
    pub window_opacity: f32,
    /// chamfer (cut corner) size in logical px ([look] chamfer)
    pub chamfer: f32,
}

pub fn state_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("SHADOW_STATE") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into())).join(".config"));
    base.join("shadow-state")
}

pub fn config() -> toml::Table {
    std::fs::read_to_string(state_dir().join("config.toml")).ok().and_then(|s| s.parse::<toml::Table>().ok()).unwrap_or_default()
}

/// No GPU driver that accelerates 3D (none, or a basic virtual one): translucent windows then cost
/// the compositor CPU time on every frame, so glass defaults to opaque. WSLg composites on Windows.
pub fn software_rendering() -> bool {
    let root = PathBuf::from(std::env::var_os("SHADOW_SYS_ROOT").filter(|v| !v.is_empty()).unwrap_or_else(|| "/".into()));
    if root.join("dev/dxg").exists() {
        return false;
    }
    let Ok(rd) = std::fs::read_dir(root.join("sys/class/drm")) else { return true };
    !rd.flatten().any(|e| {
        let n = e.file_name().to_string_lossy().to_string();
        n.starts_with("card")
            && !n.contains('-')
            && std::fs::read_link(e.path().join("device/driver")).ok().and_then(|t| t.file_name().map(|f| f.to_string_lossy().to_string())).is_some_and(|d| !matches!(d.as_str(), "vmwgfx" | "bochs-drm" | "bochs" | "cirrus" | "simpledrm" | "efifb" | "hyperv_drm" | "vboxvideo" | "qxl"))
    })
}

pub fn hex(s: &str) -> Option<Rgb> {
    let h = s.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

impl Default for Theme {
    fn default() -> Theme {
        Theme {
            bg: (0x0A, 0x0B, 0x0E),
            bg_alpha: 0.92,
            panel: (0x12, 0x14, 0x19),
            amber: (0xFF, 0xB0, 0x00),
            cyan: (0x00, 0xE5, 0xFF),
            crimson: (0xFF, 0x2A, 0x00),
            green: (0x7C, 0xD9, 0x5A),
            text: (0xE8, 0xE2, 0xD0),
            dim: (0x8A, 0x84, 0x74),
            violet: (0x9B, 0x6B, 0xFF),
            glass: (0x16, 0x1B, 0x26),
            glass_lo: (0x0C, 0x0F, 0x16),
            radius: 12.0,
            font: crate::font::DEFAULT_FAMILY.into(),
            font_pt: 11.0,
            window_opacity: 0.86,
            chamfer: 14.0,
        }
    }
}

impl Theme {
    pub fn load() -> Theme {
        let mut t = Theme::default();
        let cfg = config();
        if let Some(look) = cfg.get("look").and_then(|v| v.as_table()) {
            let col = |k: &str| look.get(k).and_then(|v| v.as_str()).and_then(hex);
            if let Some(c) = col("obsidian") { t.bg = c; }
            if let Some(c) = col("amber") { t.amber = c; }
            if let Some(c) = col("cyan") { t.cyan = c; }
            if let Some(c) = col("crimson") { t.crimson = c; }
            if let Some(f) = look.get("font").and_then(|v| v.as_str()) { t.font = f.to_string(); }
            if let Some(p) = look.get("font_pt").and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64))) {
                t.font_pt = (p as f32).clamp(6.0, 32.0);
            }
            if let Some(c) = col("violet") { t.violet = c; }
            if let Some(r) = look.get("radius").and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64))) { t.radius = (r as f32).clamp(0.0, 24.0); }
            let num = |k: &str| look.get(k).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)));
            if let Some(o) = num("window_opacity") { t.window_opacity = (o as f32).clamp(0.4, 1.0); } else if software_rendering() { t.window_opacity = 1.0; }
            if let Some(c) = num("chamfer") { t.chamfer = (c as f32).clamp(0.0, 32.0); }
            if let Some(o) = look.get("opacity").and_then(|v| v.as_float()) { t.bg_alpha = (o as f32).clamp(0.3, 1.0); }
        }
        t
    }
}
pub type TomlTable = toml::Table;
