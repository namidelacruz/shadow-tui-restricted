//! Generates niri's config.kdl for the Shadow TUI Restricted session.
//!
//! The file is fully generated from ~/.config/shadow-state/config.toml (see `shadow setup`), except
//! for everything after the USER_MARK line, which is preserved across regenerations. niri reloads
//! the file live whenever it changes.

pub const USER_MARK: &str = "// ---- your additions below this line are kept by `shadow setup niri` ----";

#[derive(Clone, Debug)]
pub struct Gen {
    pub amber: String,
    pub obsidian: String,
    pub crimson: String,
    pub gaps: u32,
    /// width presets as fractions of the monitor, e.g. [0.45, 0.65, 0.85]
    pub presets: Vec<f64>,
    pub default_preset: usize,
    pub anim_ms: u32,
    pub xkb_layout: String,
    pub xkb_options: String,
    pub repeat_delay: u32,
    pub repeat_rate: u32,
    /// scale pinned by the user per connector (displayd handles everything else live)
    pub pinned_scales: Vec<(String, f64)>,
    /// Lua key macros: (key, name) -> bind that runs `shadow-sessiond macro <key>`
    pub macros: Vec<String>,
    pub natural_scroll: bool,
}

impl Default for Gen {
    fn default() -> Gen {
        Gen {
            amber: "#FFB000".into(),
            obsidian: "#0A0B0E".into(),
            crimson: "#FF2A00".into(),
            gaps: 12,
            presets: vec![0.45, 0.65, 0.85],
            default_preset: 1,
            anim_ms: 120,
            xkb_layout: String::new(),
            xkb_options: String::new(),
            repeat_delay: 300,
            repeat_rate: 40,
            pinned_scales: vec![],
            macros: vec![],
            natural_scroll: true,
        }
    }
}

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The spec's keyboard map (CR-SPEC-SHADOW-001 §2.2), on niri actions.
pub const BINDS: &[(&str, &str)] = &[
    // apps — every feature is its own application
    ("Mod+Return", r#"spawn "shadow-term";"#),
    ("Mod+Space", r#"spawn "shadow-launch";"#),
    ("Mod+D", r#"spawn "shadow-launch";"#),
    ("Mod+N", r#"spawn "shadow-net";"#),
    ("Mod+Comma", r#"spawn "shadow-config";"#),
    ("Mod+E", r#"spawn "shadow-files";"#),
    ("Mod+U", r#"spawn "shadow-update";"#),
    ("Ctrl+Shift+Escape", r#"spawn "shadow-tasks";"#),
    ("Mod+A", r#"spawn "shadow-flyout" "apps";"#),
    ("Mod+Alt+L", r#"spawn "shadow-lock";"#),
    ("Mod+Shift+B", r#"spawn "pkill" "-USR1" "-x" "shadow-bar";"#),
    ("Mod+Shift+BackSpace", r#"spawn "shadow" "healer" "revert";"#),
    // center stage: cycle focus, step width presets, fullscreen toggle
    ("Mod+Tab", "focus-column-right-or-first;"),
    ("Mod+Shift+Tab", "focus-column-left-or-last;"),
    ("Mod+J", "focus-column-right-or-first;"),
    ("Mod+K", "focus-column-left-or-last;"),
    ("Mod+Left", "focus-column-left;"),
    ("Mod+Right", "focus-column-right;"),
    ("Mod+Up", "focus-window-or-workspace-up;"),
    ("Mod+Down", "focus-window-or-workspace-down;"),
    ("Mod+Shift+Left", "move-column-left;"),
    ("Mod+Shift+Right", "move-column-right;"),
    ("Mod+Shift+Up", "move-window-up-or-to-workspace-up;"),
    ("Mod+Shift+Down", "move-window-down-or-to-workspace-down;"),
    ("Mod+BracketRight", "switch-preset-column-width;"),
    ("Mod+BracketLeft", "switch-preset-column-width-back;"),
    ("Mod+F", "maximize-column;"),
    ("Mod+Shift+F", "fullscreen-window;"),
    ("Mod+C", "center-column;"),
    ("Mod+V", "toggle-window-floating;"),
    ("Mod+Q", "close-window;"),
    ("Mod+O", "toggle-overview;"),
    // multi-monitor
    ("Mod+Ctrl+Left", "focus-monitor-left;"),
    ("Mod+Ctrl+Right", "focus-monitor-right;"),
    ("Mod+Ctrl+Up", "focus-monitor-up;"),
    ("Mod+Ctrl+Down", "focus-monitor-down;"),
    ("Mod+Shift+Ctrl+Left", "move-column-to-monitor-left;"),
    ("Mod+Shift+Ctrl+Right", "move-column-to-monitor-right;"),
    ("Mod+Shift+Ctrl+Up", "move-column-to-monitor-up;"),
    ("Mod+Shift+Ctrl+Down", "move-column-to-monitor-down;"),
    // workspaces
    ("Mod+Page_Down", "focus-workspace-down;"),
    ("Mod+Page_Up", "focus-workspace-up;"),
    ("Mod+Shift+Page_Down", "move-column-to-workspace-down;"),
    ("Mod+Shift+Page_Up", "move-column-to-workspace-up;"),
    // screenshots
    ("Print", "screenshot;"),
    ("Ctrl+Print", "screenshot-screen;"),
    ("Alt+Print", "screenshot-window;"),
    ("Mod+Shift+Slash", "show-hotkey-overlay;"),
    ("Mod+Shift+E", "quit;"),
];

/// Hardware keys work on the lock screen too.
pub const MEDIA_BINDS: &[(&str, &str)] = &[
    ("XF86AudioRaiseVolume", r#"spawn "wpctl" "set-volume" "@DEFAULT_AUDIO_SINK@" "5%+" "-l" "1.0";"#),
    ("XF86AudioLowerVolume", r#"spawn "wpctl" "set-volume" "@DEFAULT_AUDIO_SINK@" "5%-";"#),
    ("XF86AudioMute", r#"spawn "wpctl" "set-mute" "@DEFAULT_AUDIO_SINK@" "toggle";"#),
    ("XF86AudioMicMute", r#"spawn "wpctl" "set-mute" "@DEFAULT_AUDIO_SOURCE@" "toggle";"#),
    ("XF86MonBrightnessUp", r#"spawn "shadow-displayd" "brightness" "5%+";"#),
    ("XF86MonBrightnessDown", r#"spawn "shadow-displayd" "brightness" "5%-";"#),
];

pub fn generate(g: &Gen, user_tail: &str) -> String {
    let mut o = String::new();
    let p = |o: &mut String, s: &str| {
        o.push_str(s);
        o.push('\n');
    };
    p(&mut o, "// Shadow TUI Restricted — niri configuration, generated by `shadow setup niri`.");
    p(&mut o, "// Change settings in ~/.config/shadow-state/config.toml (or shadow-config) instead of here;");
    p(&mut o, "// niri reloads this file automatically.");
    p(&mut o, "");
    p(&mut o, "input {");
    p(&mut o, "    keyboard {");
    let mut xkb = String::new();
    if !g.xkb_layout.is_empty() {
        xkb.push_str(&format!(" layout {};", q(&g.xkb_layout)));
    }
    if !g.xkb_options.is_empty() {
        xkb.push_str(&format!(" options {};", q(&g.xkb_options)));
    }
    p(&mut o, &format!("        xkb {{{} }}", xkb));
    p(&mut o, &format!("        repeat-delay {}", g.repeat_delay));
    p(&mut o, &format!("        repeat-rate {}", g.repeat_rate));
    p(&mut o, "        numlock");
    p(&mut o, "    }");
    p(&mut o, &format!("    touchpad {{ tap; dwt;{} }}", if g.natural_scroll { " natural-scroll;" } else { "" }));
    p(&mut o, "}");
    p(&mut o, "");
    p(&mut o, "// keyboard-first: the pointer disappears while typing and when idle");
    p(&mut o, "cursor { hide-when-typing; hide-after-inactive-ms 3000; }");
    p(&mut o, "prefer-no-csd");
    p(&mut o, "hotkey-overlay { skip-at-startup; }");
    p(&mut o, r#"screenshot-path "~/Pictures/Screenshots/%Y-%m-%d_%H-%M-%S.png""#);
    p(&mut o, "");
    p(&mut o, "// OpenGL only for the whole session (Arc 140V: xe driver + Mesa iris); no app picks Vulkan.");
    p(&mut o, "environment {");
    for (k, v) in [("GSK_RENDERER", "ngl"), ("QSG_RHI_BACKEND", "opengl"), ("MOZ_ENABLE_WAYLAND", "1"), ("QT_QPA_PLATFORM", "wayland"), ("SDL_VIDEODRIVER", "wayland"), ("ELECTRON_OZONE_PLATFORM_HINT", "wayland"), ("XDG_CURRENT_DESKTOP", "niri")] {
        p(&mut o, &format!("    {k} {}", q(v)));
    }
    p(&mut o, "}");
    p(&mut o, "");
    for (name, s) in &g.pinned_scales {
        p(&mut o, &format!("output {} {{ scale {:.3}; }}", q(name), s));
    }
    if !g.pinned_scales.is_empty() {
        p(&mut o, "");
    }
    p(&mut o, "layout {");
    p(&mut o, &format!("    gaps {}", g.gaps));
    p(&mut o, "    // center stage: the focused window always sits in the middle of the screen");
    p(&mut o, r#"    center-focused-column "always""#);
    p(&mut o, "    always-center-single-column");
    p(&mut o, "    preset-column-widths {");
    for w in &g.presets {
        p(&mut o, &format!("        proportion {:.3}", w));
    }
    p(&mut o, "    }");
    let d = g.presets.get(g.default_preset).copied().unwrap_or(0.65);
    p(&mut o, &format!("    default-column-width {{ proportion {:.3}; }}", d));
    p(&mut o, &format!("    focus-ring {{ width 1; active-color {}; inactive-color {}; }}", q(&g.amber), q(&format!("{}66", g.amber))));
    p(&mut o, "    border { off; }");
    p(&mut o, &format!("    background-color {}", q(&g.obsidian)));
    p(&mut o, "}");
    p(&mut o, "");
    let curve = format!("duration-ms {}; curve \"cubic-bezier\" 0.2 0.0 0.0 1.0;", g.anim_ms);
    p(&mut o, "// spec: 120 ms cubic-bezier(0.2, 0, 0, 1) for focus and size transitions");
    p(&mut o, "animations {");
    for a in ["horizontal-view-movement", "window-movement", "window-resize", "workspace-switch"] {
        p(&mut o, &format!("    {a} {{ {curve} }}"));
    }
    p(&mut o, &format!("    window-open {{ {curve} }}"));
    p(&mut o, &format!("    window-close {{ {curve} }}"));
    p(&mut o, "}");
    p(&mut o, "");
    p(&mut o, "window-rule {");
    p(&mut o, "    geometry-corner-radius 0");
    p(&mut o, "    clip-to-geometry true");
    p(&mut o, "}");
    p(&mut o, "window-rule {");
    // the launcher and the updater are ordinary windows (tiled, opaque) — never floating overlays
    // on top of other content
    p(&mut o, r#"    match app-id="^shadow-(launch|update)$""#);
    p(&mut o, "    open-floating false");
    p(&mut o, "    opacity 1.0");
    p(&mut o, "    default-column-width { proportion 0.5; }");
    p(&mut o, &format!("    focus-ring {{ active-color {}; }}", q(&g.amber)));
    p(&mut o, "}");
    p(&mut o, "layer-rule {");
    p(&mut o, r#"    match namespace="^shadow-(bar|flyout)$""#);
    p(&mut o, "    shadow { off; }");
    p(&mut o, "}");
    p(&mut o, "");
    p(&mut o, "// dock mode: lid closed with an external monitor on turns the panel off instead of locking");
    p(&mut o, "switch-events {");
    p(&mut o, r#"    lid-close { spawn "shadow-displayd" "lid-close"; }"#);
    p(&mut o, r#"    lid-open { spawn "shadow-displayd" "lid-open"; }"#);
    p(&mut o, "}");
    p(&mut o, "");
    p(&mut o, "// every Shadow feature is its own application; these start with the session");
    for app in ["shadow-sessiond", "shadow-displayd", "shadow-bar"] {
        p(&mut o, &format!("spawn-at-startup {}", q(app)));
    }
    p(&mut o, "");
    p(&mut o, "binds {");
    for (k, a) in BINDS {
        p(&mut o, &format!("    {k} {{ {a} }}"));
    }
    for (k, a) in MEDIA_BINDS {
        p(&mut o, &format!("    {k} allow-when-locked=true {{ {a} }}"));
    }
    for n in 1..=9 {
        p(&mut o, &format!("    Mod+{n} {{ focus-workspace {n}; }}"));
        p(&mut o, &format!("    Mod+Shift+{n} {{ move-column-to-workspace {n}; }}"));
    }
    for m in &g.macros {
        if BINDS.iter().any(|(k, _)| k.eq_ignore_ascii_case(m)) {
            continue; // core binds win
        }
        p(&mut o, &format!("    {m} {{ spawn \"shadow-sessiond\" \"macro\" {}; }}", q(m)));
    }
    p(&mut o, "}");
    p(&mut o, "");
    p(&mut o, USER_MARK);
    o.push_str(user_tail.trim_start_matches('\n'));
    if !o.ends_with('\n') {
        o.push('\n');
    }
    o
}

/// The user's own lines after USER_MARK in an existing file.
pub fn user_tail(existing: &str) -> String {
    existing.find(USER_MARK).map(|i| existing[i + USER_MARK.len()..].trim_start_matches('\n').to_string()).unwrap_or_default()
}

/// Shadow's niri config lives in its own state folder (the session starts niri with -c), so a
/// plain niri setup in ~/.config/niri is never touched.
pub fn path() -> std::path::PathBuf {
    if let Some(d) = std::env::var_os("SHADOW_STATE").filter(|v| !v.is_empty()) {
        return std::path::PathBuf::from(d).join("niri.kdl");
    }
    let base = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into())).join(".config"));
    base.join("shadow-state/niri.kdl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_user_tail_and_macros() {
        let g = Gen { macros: vec!["Mod+Shift+N".into(), "Mod+Q".into()], ..Gen::default() };
        let a = generate(&g, "output \"DP-1\" { position x=0 y=0; }\n");
        assert!(a.contains("Mod+Shift+N { spawn \"shadow-sessiond\" \"macro\" \"Mod+Shift+N\"; }"));
        assert_eq!(a.matches("Mod+Q {").count(), 1, "core binds are not overridden by macros");
        assert!(a.contains("proportion 0.450") && a.contains("proportion 0.850"));
        assert!(a.contains("center-focused-column \"always\""));
        let tail = user_tail(&a);
        assert_eq!(tail, "output \"DP-1\" { position x=0 y=0; }\n");
        let b = generate(&g, &tail);
        assert_eq!(a, b, "regeneration is stable");
    }
}
