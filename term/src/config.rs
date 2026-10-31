//! shadow-term configuration: ~/.config/shadow-network/term.conf (key = value).
//! ~/.config/shadow-state/term.conf (optional); unknown keys are reported by
//! `shadow-term --check-config` so a newer config never silently breaks.
use std::path::PathBuf;

#[derive(Clone)]
pub struct Config {
    pub font: String,
    pub font_size: f32,
    pub padding: u32,
    pub opacity: f32,
    pub scrollback: usize,
    pub cols: usize,
    pub rows: usize,
    pub fg: (u8, u8, u8),
    pub bg: (u8, u8, u8),
    pub cursor: (u8, u8, u8),
    pub selection: (u8, u8, u8),
    pub accent: (u8, u8, u8),
    pub accent2: (u8, u8, u8),
    pub alert: (u8, u8, u8),
    pub ok: (u8, u8, u8),
    pub dim: (u8, u8, u8),
    pub panel: (u8, u8, u8),
    pub palette: [(u8, u8, u8); 16],
    pub hud: bool,
    pub gutter: bool,
    pub scanlines: bool,
    pub boot: bool,
    pub frame: bool,
    pub cursor_style: String,
    pub unit: String,
}


impl Default for Config {
    fn default() -> Config {
        let p = [
            // tactical amber / cyan on obsidian (CR-SPEC-SHADOW-001 §2.3)
            "0a0b0e", "ff2a00", "8bd450", "ffb000", "4aa8ff", "c792ea", "00e5ff", "d8cfb8",
            "2b2a28", "ff6a45", "b5ee7c", "ffcc55", "86c5ff", "ddb8f5", "7ff2ff", "f4ecd8",
        ];
        let mut palette = [(0, 0, 0); 16];
        for (i, h) in p.iter().enumerate() {
            palette[i] = hex(h).unwrap();
        }
        Config {
            font: "JetBrainsMono Nerd Font".into(),
            font_size: 11.0,
            padding: 10,
            opacity: 0.35, // the compositor's obsidian glass plate shows through
            scrollback: 10000,
            cols: 100,
            rows: 30,
            fg: hex("d8cfb8").unwrap(),
            bg: hex("0a0b0e").unwrap(),
            cursor: hex("ffb000").unwrap(),
            selection: hex("3a2c00").unwrap(),
            accent: hex("ffb000").unwrap(),
            accent2: hex("00e5ff").unwrap(),
            alert: hex("ff2a00").unwrap(),
            ok: hex("8bd450").unwrap(),
            dim: hex("6b6455").unwrap(),
            panel: hex("121317").unwrap(),
            palette,
            hud: true,
            gutter: true,
            scanlines: true,
            boot: true,
            frame: true,
            cursor_style: "block".into(),
            unit: "SHADOW TUI".into(),
        }
    }
}

pub fn hex(s: &str) -> Option<(u8, u8, u8)> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some((p(0)?, p(2)?, p(4)?))
}

pub fn default_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())).join(".config"));
    // the state dir is where everything user-tunable lives now; the old path still works
    let new = base.join("shadow-state/term.conf");
    let old = base.join("shadow-network/term.conf");
    if !new.exists() && old.exists() { old } else { new }
}

/// Load config; returns (config, problems). Bad values keep their defaults.
pub fn load(path: &PathBuf) -> (Config, Vec<String>) {
    let mut c = Config::default();
    let mut problems = vec![];
    apply_file(&mut c, path, &mut problems, 0);
    (c, problems)
}

/// Apply one file onto `c`. `include = path` pulls in another file (depth-limited),
/// which is how the generated term.conf picks up the hand-edited term.user.conf.
fn apply_file(c: &mut Config, path: &PathBuf, problems: &mut Vec<String>, depth: u8) {
    let Ok(text) = std::fs::read_to_string(path) else {
        if depth > 0 {
            problems.push(format!("include not readable: {}", path.display()));
        }
        return;
    };
    for (n, line) in text.lines().enumerate() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') || l.starts_with('[') {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            problems.push(format!("line {}: expected key = value", n + 1));
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        let mut bad = || problems.push(format!("line {}: bad value for {k}: {v}", n + 1));
        let boolv = |v: &str| matches!(v, "true" | "yes" | "on" | "1");
        if k == "include" {
            let raw = v.trim_matches('"');
            let inc = if let Some(rest) = raw.strip_prefix("~/") {
                PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest)
            } else {
                PathBuf::from(raw)
            };
            if depth < 4 && inc.exists() {
                let mut sub = vec![];
                apply_file(c, &inc, &mut sub, depth + 1);
                problems.extend(sub.into_iter().map(|p| format!("(in {}) {p}", inc.display())));
            }
            continue;
        }
        match k {
            "font" => c.font = v.trim_matches('"').to_string(),
            "font_size" => match v.parse::<f32>() {
                Ok(x) if (5.0..=72.0).contains(&x) => c.font_size = x,
                _ => bad(),
            },
            "padding" => match v.parse::<u32>() {
                Ok(x) if x <= 100 => c.padding = x,
                _ => bad(),
            },
            "opacity" => match v.parse::<f32>() {
                Ok(x) if (0.1..=1.0).contains(&x) => c.opacity = x,
                _ => bad(),
            },
            "scrollback" => match v.parse::<usize>() {
                Ok(x) if x <= 1_000_000 => c.scrollback = x,
                _ => bad(),
            },
            "columns" => match v.parse::<usize>() {
                Ok(x) if (10..=1000).contains(&x) => c.cols = x,
                _ => bad(),
            },
            "rows" => match v.parse::<usize>() {
                Ok(x) if (3..=500).contains(&x) => c.rows = x,
                _ => bad(),
            },
            "hud" => c.hud = boolv(v),
            "gutter" => c.gutter = boolv(v),
            "scanlines" => c.scanlines = boolv(v),
            "boot_sequence" => c.boot = boolv(v),
            "frame" => c.frame = boolv(v),
            "cursor_style" if matches!(v, "block" | "beam" | "underline") => c.cursor_style = v.into(),
            "unit" => c.unit = v.trim_matches('"').chars().take(40).collect(),
            _ => {
                let colr = hex(v);
                let slot: Option<&mut (u8, u8, u8)> = match k {
                    "foreground" => Some(&mut c.fg),
                    "background" => Some(&mut c.bg),
                    "cursor" => Some(&mut c.cursor),
                    "selection" => Some(&mut c.selection),
                    "accent" => Some(&mut c.accent),
                    "accent2" => Some(&mut c.accent2),
                    "alert" => Some(&mut c.alert),
                    "ok" => Some(&mut c.ok),
                    "dim" => Some(&mut c.dim),
                    "panel" => Some(&mut c.panel),
                    _ => k.strip_prefix("color").and_then(|n| n.parse::<usize>().ok()).filter(|n| *n < 16).map(|n| &mut c.palette[n]),
                };
                match (slot, colr) {
                    (Some(s), Some(col)) => *s = col,
                    (Some(_), None) => bad(),
                    (None, _) => problems.push(format!("line {}: unknown key {k}", n + 1)),
                }
            }
        }
    }
}
