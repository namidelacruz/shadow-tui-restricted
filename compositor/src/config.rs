//! ~/.config/shadow-state/config.toml — core UI dimensions, refresh thresholds and keybindings.
//! Every field has a default, so a missing or partly broken file never stops the compositor.
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Layout {
    /// width presets of the centre window, % of the usable viewport (Mod+[ / Mod+])
    pub presets: Vec<u32>,
    pub default_preset: usize,
    /// centre window height, % of the usable height (vertically centred)
    pub height: u32,
    /// focus rotation time (cubic-bezier)
    pub anim_ms: u64,
    /// ring ellipse, fractions of the viewport width; y offset as a fraction of the height
    pub ring_rx: f64,
    pub ring_rz: f64,
    pub ring_y: f64,
    /// ring node size at depth 0, logical px
    pub node_w: i32,
    pub node_h: i32,
}
impl Default for Layout {
    fn default() -> Self {
        Layout { presets: vec![45, 65, 85], default_preset: 1, height: 84, anim_ms: 120, ring_rx: 0.41, ring_rz: 0.30, ring_y: 0.20, node_w: 250, node_h: 150 }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Look {
    pub obsidian: String,
    pub amber: String,
    pub crimson: String,
    pub cyan: String,
    pub plate_opacity: f32,
    pub node_opacity: f32,
    pub blur: bool,
    pub grid: bool,
    /// output scale (1.5 suits the 2560×1600 14" panel)
    pub scale: f64,
    pub font: String,
    pub font_px: f32,
}
impl Default for Look {
    fn default() -> Self {
        Look {
            obsidian: "#0A0B0E".into(), amber: "#FFB000".into(), crimson: "#FF2A00".into(), cyan: "#00E5FF".into(),
            plate_opacity: 0.85, node_opacity: 0.40, blur: true, grid: true, scale: 1.5, font: String::new(), font_px: 15.0,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Input {
    /// "super" (default) or "alt" (handy when nested inside another compositor)
    pub modifier: String,
    /// app ids that may receive the pointer while they are centre stage; everything else is keyboard-only
    pub pointer_apps: Vec<String>,
    pub xkb_layout: String,
    pub xkb_variant: String,
    pub xkb_options: String,
    pub repeat_delay: i32,
    pub repeat_rate: i32,
}
impl Default for Input {
    fn default() -> Self {
        Input {
            modifier: "super".into(),
            pointer_apps: ["firefox", "chromium", "google-chrome", "org.gimp.GIMP", "org.inkscape.Inkscape", "blender", "libreoffice", "org.kde.krita"].map(String::from).to_vec(),
            xkb_layout: String::new(), xkb_variant: String::new(), xkb_options: String::new(), repeat_delay: 300, repeat_rate: 40,
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Hud {
    pub enabled: bool,
    /// bar height in physical pixels (spec: 28 px on the 2560×1600 panel)
    pub height_px: i32,
}
impl Default for Hud {
    fn default() -> Self {
        Hud { enabled: true, height_px: 28 }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Display {
    pub refresh_ac: u32,
    pub refresh_battery: u32,
    /// "auto": refresh_ac on AC, refresh_battery on battery; "powerd": follow shadow-powerd's
    /// suggestion; "ac" / "battery": pin one of the two rates
    pub refresh_policy: String,
    /// preferred connector ("eDP-1"); empty = the built-in panel, else the first connected one
    pub output: String,
}
impl Default for Display {
    fn default() -> Self {
        Display { refresh_ac: 120, refresh_battery: 60, refresh_policy: "auto".into(), output: String::new() }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Lock {
    pub idle_minutes: u64,
    pub pam_service: String,
}
impl Default for Lock {
    fn default() -> Self {
        Lock { idle_minutes: 10, pam_service: "shadow-lock".into() }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Memory {
    /// put shadow-term (and so every command typed in it) inside the 20 % slice
    pub shell_in_slice: bool,
    /// program names that belong to the Shadow environment (they stay in shadow.slice)
    pub shadow_apps: Vec<String>,
}
impl Default for Memory {
    fn default() -> Self {
        Memory { shell_in_slice: false, shadow_apps: ["sakura", "shadow-config", "shadow-net", "shadow-files", "shadow-launch", "shadowctl"].map(String::from).to_vec() }
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Autostart {
    pub commands: Vec<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Config {
    pub layout: Layout,
    pub look: Look,
    pub input: Input,
    pub hud: Hud,
    pub display: Display,
    pub lock: Lock,
    pub memory: Memory,
    pub autostart: Autostart,
    pub binds: BTreeMap<String, String>,
}

pub fn default_binds() -> BTreeMap<String, String> {
    [
        ("Mod+Return", "spawn shadow-term"),
        ("Mod+T", "spawn shadow-term"),
        ("Mod+D", "spawn shadow-term --app-id=shadow-launch -e sakura launch"),
        ("Mod+Space", "spawn shadow-term --app-id=shadow-launch -e sakura launch"),
        ("Mod+A", "spawn shadow-term --app-id=shadow-launch -e sakura drawer"),
        ("Mod+E", "spawn shadow-term --app-id=shadow-files -e sakura files"),
        ("Mod+N", "spawn shadow-term --app-id=shadow-net -e sakura net"),
        ("Mod+Shift+Comma", "spawn shadow-term --app-id=shadow-config -e sakura settings"),
        ("Mod+Tab", "focus-next"),
        ("Mod+Shift+Tab", "focus-prev"),
        ("Mod+J", "focus-next"),
        ("Mod+K", "focus-prev"),
        ("Mod+BracketRight", "width-up"),
        ("Mod+BracketLeft", "width-down"),
        ("Mod+F", "fullscreen"),
        ("Mod+Q", "close"),
        ("Mod+H", "hud-toggle"),
        ("Mod+Shift+BackSpace", "healer-revert"),
        ("Mod+Alt+L", "lock"),
        ("Mod+Shift+E", "quit"),
        ("XF86AudioRaiseVolume", "spawn wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%+"),
        ("XF86AudioLowerVolume", "spawn wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-"),
        ("XF86AudioMute", "spawn wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"),
        ("XF86MonBrightnessUp", "spawn brightnessctl set 5%+"),
        ("XF86MonBrightnessDown", "spawn brightnessctl set 5%-"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            layout: Layout::default(), look: Look::default(), input: Input::default(), hud: Hud::default(), display: Display::default(),
            lock: Lock::default(), memory: Memory::default(), autostart: Autostart::default(), binds: default_binds(),
        }
    }
}

pub fn state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("SHADOW_STATE").filter(|v| !v.is_empty()) {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("shadow-state")
}

/// (config, problem) — a broken file falls back to defaults and reports why
pub fn load(path: Option<&std::path::Path>) -> (Config, Option<String>) {
    let p = path.map(PathBuf::from).unwrap_or_else(|| state_dir().join("config.toml"));
    let Ok(text) = std::fs::read_to_string(&p) else { return (Config::default(), None) };
    match toml::from_str::<Config>(&text) {
        Ok(mut c) => {
            // user binds extend the defaults; "none" removes one
            let mut b = default_binds();
            for (k, v) in std::mem::take(&mut c.binds) {
                if v == "none" { b.remove(&k); } else { b.insert(k, v); }
            }
            c.binds = b;
            c.sanitize();
            (c, None)
        }
        Err(e) => (Config::default(), Some(format!("{}: {e}", p.display()))),
    }
}

impl Config {
    fn sanitize(&mut self) {
        let l = &mut self.layout;
        l.presets.retain(|p| (20..=100).contains(p));
        if l.presets.is_empty() {
            l.presets = vec![45, 65, 85];
        }
        l.default_preset = l.default_preset.min(l.presets.len() - 1);
        l.height = l.height.clamp(40, 100);
        l.anim_ms = l.anim_ms.min(2000);
        self.look.scale = self.look.scale.clamp(0.5, 4.0);
        self.look.plate_opacity = self.look.plate_opacity.clamp(0.2, 1.0);
        self.look.node_opacity = self.look.node_opacity.clamp(0.05, 1.0);
        self.look.font_px = self.look.font_px.clamp(8.0, 40.0);
        self.hud.height_px = self.hud.height_px.clamp(16, 96);
    }
}

pub fn rgb(hex: &str) -> [f32; 3] {
    let h = hex.trim_start_matches('#');
    let p = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0) as f32 / 255.0;
    if h.len() >= 6 { [p(0), p(2), p(4)] } else { [1.0, 0.69, 0.0] }
}

#[cfg(test)]
mod t {
    #[test]
    fn partial_and_broken() {
        let c: super::Config = toml::from_str("[layout]\npresets=[50, 999, 70]\n[binds]\n\"Mod+Q\" = \"none\"\n").unwrap();
        assert_eq!(c.layout.presets, vec![50, 999, 70]);
        let dir = std::env::temp_dir().join(format!("sc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("c.toml"), "[layout]\npresets=[50, 999, 70]\n[binds]\n\"Mod+Q\" = \"none\"\n").unwrap();
        let (c, e) = super::load(Some(&dir.join("c.toml")));
        assert!(e.is_none());
        assert_eq!(c.layout.presets, vec![50, 70]);
        assert!(!c.binds.contains_key("Mod+Q"));
        std::fs::write(dir.join("c.toml"), "[layout\n").unwrap();
        let (c, e) = super::load(Some(&dir.join("c.toml")));
        assert!(e.is_some());
        assert_eq!(c.layout.presets, vec![45, 65, 85]);
        assert_eq!(super::rgb("#FFB000"), [1.0, 176.0 / 255.0, 0.0]);
    }
}
