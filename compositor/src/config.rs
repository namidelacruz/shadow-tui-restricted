//! ~/.config/shadow-state/config.toml — core UI dimensions, refresh thresholds and keybindings.
//! Every field has a default, so a missing or partly broken file never stops the compositor.
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Layout {
    /// main-tile share of the app area's width with 2–3 apps, % (Mod+[ / Mod+] step through them)
    pub presets: Vec<u32>,
    pub default_preset: usize,
    /// kept for 8.x configs (unused)
    pub height: u32,
    /// window move / resize time (cubic-bezier)
    pub anim_ms: u64,
    /// open apps at most; one more minimises the oldest
    pub max_open: usize,
    /// space between tiles and around the app area, logical px
    pub gap: u32,
    /// edge of a minimised app's square tile, logical px
    pub tile_px: u32,
    /// kinetic unfolding of windows opening / closing / minimising, ms (0 = off)
    pub unfold_ms: u64,
}
impl Default for Layout {
    fn default() -> Self {
        Layout { presets: vec![50, 60, 70], default_preset: 1, height: 84, anim_ms: 160, max_open: 4, gap: 14, tile_px: 128, unfold_ms: 560 }
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
    /// the travelling light on the background lines: "on" (30 fps on mains, 12 on battery),
    /// "low" (12 fps), "off" (still)
    pub motion: String,
}
impl Default for Look {
    fn default() -> Self {
        Look {
            obsidian: "#0E0C09".into(), amber: "#D4AA5A".into(), crimson: "#C84B31".into(), cyan: "#F0DB9C".into(),
            plate_opacity: 0.85, node_opacity: 0.40, blur: true, grid: true, scale: 1.5, font: String::new(), font_px: 15.0, motion: "on".into(),
        }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Input {
    /// "super" (default) or "alt" (handy when nested inside another compositor)
    pub modifier: String,
    /// "all" (default): the mouse works everywhere — the hub, the launcher, every window, the ring;
    /// "apps": keyboard-first, only the centre windows listed in pointer_apps get the pointer
    /// (Shadow's own layers — hub, launcher, menus — always do)
    pub pointer: String,
    /// app ids that may receive the pointer in "apps" mode
    pub pointer_apps: Vec<String>,
    /// hide the cursor while typing; it comes back on the next move
    pub hide_cursor_when_typing: bool,
    /// touchpad: tap to click (on), natural (reversed) scrolling (off), pointer speed -1..1 (0)
    pub tap_to_click: bool,
    pub natural_scroll: bool,
    pub pointer_speed: f64,
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
            pointer: "all".into(),
            hide_cursor_when_typing: true,
            tap_to_click: true,
            natural_scroll: false,
            pointer_speed: 0.0,
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
        Hud { enabled: false, height_px: 28 }
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
    /// with a second monitor: where new apps open — "external" (the other screen), "focused",
    /// "primary"
    pub open_apps_on: String,
    /// use every connected monitor (false: only the preferred one, as before 10.0)
    pub all_outputs: bool,
}
impl Default for Display {
    fn default() -> Self {
        Display { refresh_ac: 120, refresh_battery: 60, refresh_policy: "auto".into(), output: String::new(), open_apps_on: "external".into(), all_outputs: true }
    }
}

/// [shell]: the desktop around the apps. "web" (10.0): the taskbar, start menu, widgets and quick
/// settings as local web pages in shadow-panel windows; "glyph": the 9.x edge columns.
#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Shell {
    pub mode: String,
    /// title bars with minimise / maximise / close over every window
    pub titlebars: bool,
}
impl Default for Shell {
    fn default() -> Self {
        Shell { mode: "web".into(), titlebars: true }
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
        Memory { shell_in_slice: false, shadow_apps: ["shadow-glyph", "shadow-webd", "shadow-settings", "shadow-net", "shadow-files", "shadow-launch", "shadow-prefs", "shadow-craft", "shadowctl"].map(String::from).to_vec() }
    }
}

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Autostart {
    pub commands: Vec<String>,
}
impl Default for Autostart {
    fn default() -> Self {
        Autostart { commands: default_autostart() }
    }
}

/// [defaults] from Shadow's config.toml (shadow-prefs → DEFAULTS): what Mod+Return and Mod+E start
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Defaults {
    pub terminal_cmd: String,
    pub files_cmd: String,
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
    pub defaults: Defaults,
    pub shell: Shell,
    pub binds: BTreeMap<String, String>,
}

/// The same keys as Shadow on niri (the old niri config generator), on this compositor's actions
pub fn default_binds() -> BTreeMap<String, String> {
    [
        ("Mod+Return", "spawn shadow-term"),
        ("Mod+T", "spawn shadow-term"),
        ("Mod+D", "spawn shadow-launch"),
        // the shell (10.0): start menu, widgets on the left, quick settings on the right
        ("Mod+Space", "shell start"),
        ("Mod+W", "shell widgets"),
        ("Mod+A", "shell quick"),
        ("Mod+E", "spawn shadow-files"),
        ("Mod+N", "spawn shadow-net"),
        ("Mod+Comma", "spawn shadow-settings settings"),
        ("Mod+U", "spawn shadow-update"),
        ("Mod+Shift+W", "spawn shadow-settings look"),
        ("Mod+Shift+H", "spawn shadow-settings fix"),
        ("Ctrl+Shift+Escape", "spawn shadow-tasks"),
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
        ("Mod+Shift+F", "fullscreen"),
        // the taskbar: hide / show
        ("Mod+Shift+B", "shell toggle"),
        ("Mod+P", "spawn shadow-settings defaults"),
        // minimise to the grid · bring the last one back · focused app to the main tile
        ("Mod+M", "minimize"),
        ("Mod+Shift+M", "restore"),
        // Windows-style: maximise · restore down (then minimise) · bring the last minimised back
        ("Mod+Up", "maximize"),
        ("Mod+Down", "unmaximize"),
        ("Mod+Shift+Up", "restore"),
        // workspaces (each screen has nine) and screens
        ("Mod+Ctrl+Right", "workspace-next"),
        ("Mod+Ctrl+Left", "workspace-prev"),
        ("Mod+Ctrl+Shift+Right", "move-to-workspace-next"),
        ("Mod+Ctrl+Shift+Left", "move-to-workspace-prev"),
        ("Mod+Shift+Right", "move-to-screen-next"),
        ("Mod+Shift+Left", "move-to-screen-prev"),
        ("Mod+O", "focus-screen-next"),
        ("Mod+Shift+Return", "promote"),
        // the hotkey tutorial (first login opens it by itself)
        ("Mod+Shift+Slash", "tutorial"),
        ("Mod+F1", "tutorial"),
        ("XF86AudioRaiseVolume", "spawn wpctl set-volume -l 1.0 @DEFAULT_AUDIO_SINK@ 5%+"),
        ("XF86AudioLowerVolume", "spawn wpctl set-volume @DEFAULT_AUDIO_SINK@ 5%-"),
        ("XF86AudioMute", "spawn wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle"),
        ("XF86MonBrightnessUp", "spawn shadow-displayd brightness 5%+"),
        ("XF86MonBrightnessDown", "spawn shadow-displayd brightness 5%-"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .chain((1..=9).flat_map(|n| [(format!("Mod+{n}"), format!("workspace {n}")), (format!("Mod+Shift+{n}"), format!("move-to-workspace {n}"))]))
    .collect()
}

/// 9.0: the glyph shell is the desktop (the 8.x [hub] switch is gone); kept for the HUD default
pub fn hub_enabled() -> bool {
    true
}

#[allow(dead_code)]
fn hub_enabled_8x() -> bool {
    let t = std::fs::read_to_string(state_dir().join("config.toml")).unwrap_or_default();
    let mut sec = String::new();
    for l in t.lines() {
        let l = l.trim();
        if l.starts_with('[') {
            sec = l.trim_matches(|c| c == '[' || c == ']').to_string();
        } else if sec == "hub" && l.starts_with("enabled") {
            return !l.contains("false");
        }
    }
    true
}

pub fn default_autostart() -> Vec<String> {
    // the local web service (settings app + shell pages) and the shell itself ([shell] mode
    // decides which: the web panels, or the 9.x glyph columns — shadow-shell picks, and falls
    // back to the glyph columns when the web panels can't start)
    vec!["shadow-webd".into(), "shadow-shell".into()]
}

impl Default for Config {
    fn default() -> Self {
        Config {
            layout: Layout::default(), look: Look::default(), input: Input::default(), hud: Hud::default(), display: Display::default(),
            lock: Lock::default(), memory: Memory::default(), autostart: Autostart::default(), defaults: Defaults::default(), shell: Shell::default(), binds: default_binds(),
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
            // the default terminal / file manager chosen in shadow-prefs, unless the key was rebound
            let stock = default_binds();
            let q = |v: &str| v.trim().trim_matches('"').trim().to_string();
            let mut put = |k: &str, cmd: &str| {
                if !cmd.is_empty() && b.get(k) == stock.get(k) {
                    b.insert(k.into(), format!("spawn {cmd}"));
                }
            };
            put("Mod+Return", &q(&c.defaults.terminal_cmd));
            put("Mod+T", &q(&c.defaults.terminal_cmd));
            put("Mod+E", &q(&c.defaults.files_cmd));
            c.binds = b;
            c.sanitize();
            (c, None)
        }
        Err(e) => (Config::default(), Some(format!("{}: {e}", p.display()))),
    }
}

impl Config {
    fn sanitize(&mut self) {
        // 8.x default colours left in an old config.toml switch to the 9.0 gold theme
        let d = Look::default();
        for (v, old, new) in [(&mut self.look.obsidian, "#0a0b0e", &d.obsidian), (&mut self.look.amber, "#ffb000", &d.amber), (&mut self.look.crimson, "#ff2a00", &d.crimson), (&mut self.look.cyan, "#00e5ff", &d.cyan)] {
            if v.trim().eq_ignore_ascii_case(old) {
                *v = new.clone();
            }
        }
        let l = &mut self.layout;
        l.presets.retain(|p| (20..=100).contains(p));
        if l.presets.is_empty() {
            l.presets = vec![50, 60, 70];
        }
        l.default_preset = l.default_preset.min(l.presets.len() - 1);
        l.max_open = l.max_open.clamp(1, 9);
        l.gap = l.gap.min(48);
        l.tile_px = l.tile_px.clamp(64, 256);
        l.unfold_ms = l.unfold_ms.min(2000);
        if !["on", "low", "off"].contains(&self.look.motion.as_str()) {
            self.look.motion = "on".into();
        }
        l.height = l.height.clamp(40, 100);
        l.anim_ms = l.anim_ms.min(2000);
        self.look.scale = self.look.scale.clamp(0.5, 4.0);
        self.look.plate_opacity = self.look.plate_opacity.clamp(0.2, 1.0);
        self.look.node_opacity = self.look.node_opacity.clamp(0.05, 1.0);
        self.look.font_px = self.look.font_px.clamp(8.0, 40.0);
        self.hud.height_px = self.hud.height_px.clamp(16, 96);
        if !["all", "apps"].contains(&self.input.pointer.as_str()) {
            self.input.pointer = "all".into();
        }
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
        assert_eq!(c.layout.presets, vec![50, 60, 70]);
        assert_eq!(super::rgb("#FFB000"), [1.0, 176.0 / 255.0, 0.0]);
        // Shadow's own config.toml (the sections every Shadow program shares) still loads, and the
        // DEFAULTS page's terminal / file manager drive Mod+Return / Mod+E
        let shadow = format!("{}\n[defaults]\nterminal_cmd = \"foot\"\nfiles_cmd = \"nautilus --new-window\"\n[hub]\nenabled = true\n[display.scale]\n\"eDP-1\" = 1.5\n", super::shadow_core_like());
        std::fs::write(dir.join("c.toml"), shadow).unwrap();
        let (c, e) = super::load(Some(&dir.join("c.toml")));
        assert!(e.is_none(), "{e:?}");
        assert_eq!(c.binds["Mod+Return"], "spawn foot");
        assert_eq!(c.binds["Mod+E"], "spawn nautilus --new-window");
        assert_eq!(c.input.pointer, "all");
        assert_eq!(c.binds["Mod+Shift+Slash"], "tutorial");
    }
}

#[cfg(test)]
fn shadow_core_like() -> &'static str {
    "[layout]\npresets = [45, 65, 85]\ngaps = 12\n[look]\nopacity = 0.92\nfont_pt = 11\n[display]\nrefresh_policy = \"auto\"\n[bar]\nwidth = 250\n[input]\nxkb_layout = \"us\"\n[session]\nlock_after_min = 10\ncompositor = \"shadow\"\n[memory]\nshell_in_slice = false\n[power]\nlearning = true\n[updates]\nbranch = \"main\"\n[sync]\nage_recipient = \"\"\n"
}
