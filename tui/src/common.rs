//! Paths, themes, palettes and small helpers shared by all three modes.
use crate::json::{self, Value};
use crate::term::{Color, Style};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const ROLES: [&str; 12] = [
    "bg", "bg_alt", "surface", "fg", "fg_dim", "accent", "accent2", "accent3", "alert", "warn", "ok", "inactive",
];

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}
fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var(var).ok().filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(fallback))
}
pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("shadow-state")
}
pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("shadow-tui")
}
pub fn user_data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("shadow-tui")
}
/// Program data: $SHADOWNET_SHARE, then the system package, then a legacy user install.
pub fn share_dir() -> PathBuf {
    if let Ok(s) = std::env::var("SHADOW_SHARE").or_else(|_| std::env::var("SHADOWNET_SHARE")) {
        if !s.is_empty() {
            return PathBuf::from(s);
        }
    }
    let sys = PathBuf::from("/opt/shadow-tui/share");
    if sys.join("themes").is_dir() {
        return sys;
    }
    user_data_dir()
}
pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}
#[allow(dead_code)]
pub fn keybinds_path() -> PathBuf {
    config_dir().join("keybinds.conf")
}

pub fn read_json(p: &Path) -> Option<Value> {
    fs::read_to_string(p).ok().and_then(|s| json::parse(&s).ok())
}

/// Atomic write: temp file in the same dir, then rename.
pub fn atomic_write(p: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    let tmp = p.with_file_name(format!(".{}.tmp{}", p.file_name().unwrap().to_string_lossy(), std::process::id()));
    fs::write(&tmp, text)?;
    fs::rename(&tmp, p)
}

pub fn valid_hex(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Run a command, capture stdout+stderr. Never panics; missing programs -> (127, msg).
pub fn cmd_out(args: &[&str]) -> (i32, String) {
    match Command::new(args[0]).args(&args[1..]).stdin(Stdio::null()).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).to_string();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.code().unwrap_or(1), s)
        }
        Err(e) => (127, format!("{}: {}", args[0], e)),
    }
}

extern "C" {
    fn setsid() -> i32;
}

/// Start a shell command fully detached (survives the terminal closing).
pub fn spawn_detached(cmd: &str) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(cmd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    unsafe {
        c.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    c.spawn().map(|_| ())
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub inspired: String,
    pub desc: String,
    pub bar_style: String,
    pub colors: Vec<(String, String)>, // role -> hex
    #[allow(dead_code)]
    pub terminal: Vec<String>,
}

impl Theme {
    pub fn hex(&self, role: &str) -> String {
        self.colors.iter().find(|(r, _)| r == role).map(|(_, h)| h.clone()).unwrap_or_else(|| "#808080".into())
    }
    pub fn c(&self, role: &str) -> Color {
        Color::hex(&self.hex(role))
    }
    pub fn set(&mut self, role: &str, hex: &str) {
        if let Some(slot) = self.colors.iter_mut().find(|(r, _)| r == role) {
            slot.1 = hex.to_string();
        }
    }
    pub fn from_json(v: &Value) -> Option<Theme> {
        let colors: Vec<(String, String)> = ROLES
            .iter()
            .map(|r| (r.to_string(), v.path_str(&["colors", r]).unwrap_or_default()))
            .collect();
        if colors.iter().any(|(_, h)| !valid_hex(h)) {
            return None;
        }
        let terminal: Vec<String> = match v.get("terminal") {
            Some(Value::Arr(a)) => a.iter().filter_map(|x| x.str().map(String::from)).collect(),
            _ => vec![],
        };
        Some(Theme {
            id: v.path_str(&["id"])?,
            name: v.path_str(&["name"]).unwrap_or_default(),
            inspired: v.path_str(&["inspired_by"]).unwrap_or_default(),
            desc: v.path_str(&["description"]).unwrap_or_default(),
            bar_style: v.path_str(&["bar_style"]).unwrap_or_else(|| "frame".into()),
            colors,
            terminal,
        })
    }
    pub fn fallback() -> Theme {
        let c = [
            "#070a06", "#0d130b", "#141d11", "#c8d6bf", "#5f7056", "#9fd356", "#f5a623", "#4fd1c5", "#ff3b30", "#ffcc00",
            "#9fd356", "#1f2a1c",
        ];
        Theme {
            id: "nightwatch".into(),
            name: "Nightwatch".into(),
            inspired: "Ghost in the Shell (tactical)".into(),
            desc: String::new(),
            bar_style: "tactical".into(),
            colors: ROLES.iter().zip(c.iter()).map(|(r, h)| (r.to_string(), h.to_string())).collect(),
            terminal: vec![],
        }
    }

    /// 16 Linux-console palette slots. Slots 0-7 double as background colours.
    pub fn vt_slots(&self) -> [Color; 16] {
        let r = |n: &str| self.c(n);
        [
            r("bg"), r("alert"), r("surface"), r("accent"), r("accent2"), r("accent3"), r("bg_alt"), r("fg"),
            r("inactive"), r("alert"), r("ok"), r("warn"), r("fg_dim"), r("accent3"), r("accent2"), r("fg"),
        ]
    }

    /// Border glyphs by bar style. `safe` = Linux console (CP437 subset).
    pub fn glyphs(&self, safe: bool) -> [char; 6] {
        match (self.bar_style.as_str(), safe) {
            ("bracket", _) => ['╔', '╗', '╚', '╝', '═', '║'],
            ("slash", false) => ['┏', '┓', '┗', '┛', '━', '┃'],
            _ => ['┌', '┐', '└', '┘', '─', '│'],
        }
    }
}

pub fn theme_dirs() -> Vec<PathBuf> {
    vec![config_dir().join("themes"), share_dir().join("themes"), PathBuf::from("/opt/shadow-tui/share/themes")]
}

pub fn list_themes() -> Vec<Theme> {
    let mut out: Vec<Theme> = vec![];
    for d in theme_dirs() {
        let mut files: Vec<PathBuf> = match fs::read_dir(&d) {
            Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map(|x| x == "json").unwrap_or(false)).collect(),
            Err(_) => continue,
        };
        files.sort();
        for f in files {
            if let Some(t) = read_json(&f).as_ref().and_then(Theme::from_json) {
                if !out.iter().any(|o| o.id == t.id) {
                    out.push(t);
                }
            }
        }
    }
    if out.is_empty() {
        out.push(Theme::fallback());
    }
    out
}

pub fn find_theme(id: &str) -> Theme {
    let all = list_themes();
    all.iter().find(|t| t.id == id).cloned().unwrap_or_else(|| all[0].clone())
}

/// The tactical theme with the [look] colours from config.toml applied (amber → accent,
/// cyan → accent2, crimson → alert, obsidian → bg). `[look] tui_theme = "nightwatch"` picks another.
pub fn active_theme_tactical() -> Theme {
    let text = fs::read_to_string(config_dir().join("config.toml")).unwrap_or_default();
    let get = |k: &str| crate::settings::get_in(&text, "look", k);
    let mut t = find_theme(&get("tui_theme").unwrap_or_else(|| "tactical".into()));
    for (key, role) in [("amber", "accent"), ("cyan", "accent2"), ("crimson", "alert"), ("obsidian", "bg")] {
        if let Some(h) = get(key).map(|h| h.to_lowercase()) {
            if valid_hex(&h) {
                t.set(role, &h);
            }
        }
    }
    t
}

/// Theme for the sakura apps (settings.json is gone; kept for the call sites).
pub fn active_theme(settings: &Value) -> Theme {
    let mut t = active_theme_tactical();
    for (role, v) in settings.path(&["color_overrides"]).map(|v| v.entries().to_vec()).unwrap_or_default() {
        if let Some(h) = v.str() {
            if valid_hex(h) {
                t.set(&role, h);
            }
        }
    }
    t
}

/// Common styles derived from a theme.
pub struct Ui {
    pub base: Style,
    pub dim: Style,
    pub accent: Style,
    pub accent2: Style,
    pub panel: Style,
    pub sel: Style,
    pub alert: Style,
    pub ok: Style,
    pub input: Style,
    pub glyphs: [char; 6],
    pub theme: Theme,
}

impl Ui {
    pub fn new(t: &Theme, safe: bool) -> Ui {
        let bg = t.c("bg");
        let panel_bg = t.c("bg_alt");
        Ui {
            base: Style::new(t.c("fg"), bg),
            dim: Style::new(t.c("fg_dim"), bg),
            accent: Style::new(t.c("accent"), bg).bold(),
            accent2: Style::new(t.c("accent2"), bg).bold(),
            panel: Style::new(t.c("fg"), panel_bg),
            sel: Style::new(t.c("bg"), t.c("accent")).bold(),
            alert: Style::new(t.c("alert"), bg).bold(),
            ok: Style::new(t.c("ok"), bg),
            input: Style::new(t.c("fg"), t.c("surface")),
            glyphs: t.glyphs(safe),
            theme: t.clone(),
        }
    }
}

/// Collapse long paths for display.
pub fn short(p: &str) -> String {
    let h = home().to_string_lossy().to_string();
    if !h.is_empty() && h != "/" && p.starts_with(&h) {
        format!("~{}", &p[h.len()..])
    } else {
        p.to_string()
    }
}
