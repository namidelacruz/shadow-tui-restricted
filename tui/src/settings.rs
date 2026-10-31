//! shadow-config — the live settings matrix for ~/.config/shadow-state/config.toml.
//!
//! Every row maps to one key. A change is written in place (comments and order kept), pushed
//! with `shadowctl config-reload`, and rolled back if the compositor reports a problem.
//! Power rows talk to shadow-powerd; the Security page only shows shadow-sec (it is optional
//! and changed with sudo, never from here). Mouse: click a page or a row, wheel scrolls.
use crate::common::{atomic_write, cmd_out, find_theme, Ui};
use crate::term::{Input, Key, Style, Term};
use std::path::PathBuf;

#[derive(Clone, Copy)]
enum Kind {
    Num { min: f64, max: f64, step: f64, float: bool },
    Bool,
    Choice(&'static [&'static str]),
    Text,
    Color,
}

struct Row {
    section: &'static str,
    key: &'static str,
    label: &'static str,
    kind: Kind,
    default: &'static str,
}

const fn r(section: &'static str, key: &'static str, label: &'static str, kind: Kind, default: &'static str) -> Row {
    Row { section, key, label, kind, default }
}
const fn num(min: f64, max: f64, step: f64, float: bool) -> Kind {
    Kind::Num { min, max, step, float }
}

const PAGES: [(&str, &str); 9] = [
    ("Centre stage", "width presets, height, ring geometry, rotation time"),
    ("Look", "colours, glass plate, blur shader, grid, scale, font"),
    ("Input", "keyboard-first: modifier key and which apps may use the pointer"),
    ("HUD + display", "bezel HUD and the AC / battery refresh rates"),
    ("Power + NPU", "shadow-powerd mode, learning, predictive tuning"),
    ("Lock + memory", "idle lock and the 20 % memory ceiling"),
    ("Healer + AI", "self-correction mode (approval or automatic) and the local model"),
    ("Updates + sync", "update source and the GitHub state repository"),
    ("Security", "optional shadow-sec status (read-only here)"),
];

fn rows(page: usize) -> Vec<Row> {
    match page {
        0 => vec![
            r("layout", "default_preset", "Default width preset (1 = narrowest)", num(0.0, 2.0, 1.0, false), "1"),
            r("layout", "height", "Centre window height (%)", num(40.0, 100.0, 2.0, false), "84"),
            r("layout", "anim_ms", "Focus rotation (ms, cubic-bezier)", num(0.0, 600.0, 20.0, false), "120"),
            r("layout", "ring_rx", "Ring width (fraction of the screen)", num(0.2, 0.6, 0.01, true), "0.41"),
            r("layout", "ring_rz", "Ring depth", num(0.1, 0.6, 0.01, true), "0.3"),
            r("layout", "ring_y", "Ring height below centre", num(0.0, 0.4, 0.01, true), "0.2"),
            r("layout", "node_w", "Ring node width (px)", num(150.0, 420.0, 10.0, false), "250"),
        ],
        1 => vec![
            r("look", "amber", "Tactical amber", Kind::Color, "#FFB000"),
            r("look", "cyan", "Cyan", Kind::Color, "#00E5FF"),
            r("look", "crimson", "Crimson", Kind::Color, "#FF2A00"),
            r("look", "obsidian", "Obsidian", Kind::Color, "#0A0B0E"),
            r("look", "plate_opacity", "Centre plate opacity", num(0.2, 1.0, 0.05, true), "0.85"),
            r("look", "node_opacity", "Ring wireframe opacity", num(0.05, 1.0, 0.05, true), "0.4"),
            r("look", "blur", "Blur shader behind the centre window", Kind::Bool, "true"),
            r("look", "grid", "Tactical floor grid", Kind::Bool, "true"),
            r("look", "scale", "Output scale (next login)", num(1.0, 3.0, 0.25, true), "1.5"),
            r("look", "font_px", "HUD / label font size", num(10.0, 24.0, 1.0, false), "15"),
        ],
        2 => vec![
            r("input", "modifier", "Mod key", Kind::Choice(&["super", "alt"]), "super"),
            r("input", "xkb_layout", "Keyboard layout (e.g. us,ca)", Kind::Text, ""),
            r("input", "repeat_rate", "Key repeat rate", num(10.0, 80.0, 5.0, false), "40"),
        ],
        3 => vec![
            r("hud", "enabled", "Bezel HUD", Kind::Bool, "true"),
            r("hud", "height_px", "HUD bar height (px)", num(16.0, 64.0, 2.0, false), "28"),
            r("display", "refresh_ac", "Refresh on AC (Hz)", num(48.0, 240.0, 12.0, false), "120"),
            r("display", "refresh_battery", "Refresh on battery (Hz)", num(30.0, 120.0, 12.0, false), "60"),
            r("display", "refresh_policy", "Refresh policy", Kind::Choice(&["auto", "powerd", "ac", "battery"]), "auto"),
        ],
        4 => vec![r("power", "learning", "Learn from my power choices", Kind::Bool, "true")],
        5 => vec![
            r("lock", "idle_minutes", "Lock after idle (minutes, 0 = never)", num(0.0, 120.0, 1.0, false), "10"),
            r("memory", "shell_in_slice", "Commands in shadow-term share the 20 % ceiling", Kind::Bool, "false"),
        ],
        6 => vec![],
        7 => vec![
            r("updates", "repo", "Update source (git URL)", Kind::Text, ""),
            r("updates", "branch", "Branch", Kind::Text, "main"),
            r("updates", "check_on_login", "Check for updates at login", Kind::Bool, "true"),
            r("sync", "age_recipient", "secrets.age recipient (empty = ~/.ssh/id_ed25519.pub)", Kind::Text, ""),
        ],
        _ => vec![],
    }
}

fn config_path() -> PathBuf {
    if let Some(d) = std::env::var_os("SHADOW_STATE").filter(|v| !v.is_empty()) {
        return PathBuf::from(d).join("config.toml");
    }
    crate::common::config_dir().join("config.toml")
}

/// value of [section] key (strings unquoted)
pub fn get_in(text: &str, section: &str, key: &str) -> Option<String> {
    let mut cur = String::new();
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            cur = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        if cur != section || t.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            if k.trim().trim_matches('"') == key {
                let v = v.split(" #").next().unwrap_or("").trim();
                return Some(if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') { v[1..v.len() - 1].to_string() } else { v.to_string() });
            }
        }
    }
    None
}

/// set [section] key = literal, keeping everything else; a commented default line is replaced
pub fn set_in(text: &str, section: &str, key: &str, literal: &str) -> String {
    let mut out: Vec<String> = vec![];
    let (mut cur, mut done, mut seen) = (String::new(), false, false);
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if cur == section && !done {
                while out.last().map_or(false, |x: &String| x.trim().is_empty()) {
                    out.pop();
                }
                out.push(format!("{key} = {literal}"));
                out.push(String::new());
                done = true;
            }
            cur = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            seen |= cur == section;
        } else if cur == section && !done {
            let body = t.trim_start_matches('#').trim();
            if let Some((k, _)) = body.split_once('=') {
                if k.trim().trim_matches('"') == key {
                    out.push(format!("{key} = {literal}"));
                    done = true;
                    continue;
                }
            }
        }
        out.push(l.to_string());
    }
    if !done {
        if !seen {
            out.push(String::new());
            out.push(format!("[{section}]"));
        }
        out.push(format!("{key} = {literal}"));
    }
    out.join("\n") + "\n"
}

struct App {
    ui: Ui,
    page: usize,
    sel: usize,
    side: bool,
    text: String,
    edit: Option<Input>,
    status: (String, bool),
    quit: bool,
    power: String,
    sec: String,
    healer: String,
}

impl App {
    fn value(&self, r: &Row) -> String {
        get_in(&self.text, r.section, r.key).unwrap_or_else(|| r.default.to_string())
    }

    fn write(&mut self, r: &Row, literal: String) {
        let before = self.text.clone();
        let after = set_in(&self.text, r.section, r.key, &literal);
        if atomic_write(&config_path(), &after).is_err() {
            self.status = (format!("cannot write {}", config_path().display()), false);
            return;
        }
        self.text = after;
        let (code, out) = cmd_out(&["shadowctl", "config-reload"]);
        if code == 127 || out.contains("not running") {
            self.status = (format!("{} = {literal} (saved; applies when the session starts)", r.key), true);
        } else if out.starts_with("ok") {
            self.status = (format!("{} = {literal} · applied live", r.key), true);
        } else {
            // the compositor rejected the file: put the previous one back
            let _ = atomic_write(&config_path(), &before);
            self.text = before;
            let _ = cmd_out(&["shadowctl", "config-reload"]);
            self.status = (format!("rejected, rolled back: {}", out.trim()), false);
        }
    }

    fn adjust(&mut self, dir: f64) {
        let rs = rows(self.page);
        let Some(r) = rs.get(self.sel) else { return };
        let v = self.value(r);
        let lit = match r.kind {
            Kind::Num { min, max, step, float } => {
                let x = (v.parse::<f64>().unwrap_or(min) + dir * step).clamp(min, max);
                if float { format!("{:.2}", x).trim_end_matches('0').trim_end_matches('.').to_string() } else { format!("{}", x.round() as i64) }
            }
            Kind::Bool => (v != "true").to_string(),
            Kind::Choice(opts) => {
                let i = opts.iter().position(|o| *o == v).unwrap_or(0) as i64;
                let n = opts.len() as i64;
                format!("\"{}\"", opts[((i + dir as i64).rem_euclid(n)) as usize])
            }
            Kind::Text | Kind::Color => {
                self.edit = Some(Input::with(&v));
                return;
            }
        };
        self.write(r, lit);
    }

    fn page_actions(&mut self, k: &Key) -> bool {
        match (self.page, k) {
            (4, Key::Char(c)) if ('1'..='6').contains(c) => {
                let mode = ["auto", "performance", "balanced", "quiet", "cool", "low-power"][(*c as u8 - b'1') as usize];
                let (_, out) = cmd_out(&["shadow-powerd", "mode", mode]);
                self.status = (out.lines().last().unwrap_or("").to_string(), true);
                self.refresh();
                true
            }
            (6, Key::Char('p')) => {
                self.status = ("healer proposals: shadow-healer list / show N / sudo shadow-healer approve N".into(), true);
                true
            }
            (_, Key::Char('u')) => {
                self.refresh();
                self.status = ("refreshed".into(), true);
                true
            }
            _ => false,
        }
    }

    fn refresh(&mut self) {
        self.power = cmd_out(&["shadow-powerd", "status"]).1;
        self.sec = cmd_out(&["shadow-sec", "status"]).1;
        self.healer = cmd_out(&["shadow", "healer", "status"]).1;
    }

    fn key(&mut self, k: Key) {
        if let Some(inp) = self.edit.as_mut() {
            match k {
                Key::Enter => {
                    let v = inp.buf.clone();
                    self.edit = None;
                    let rs = rows(self.page);
                    if let Some(r) = rs.get(self.sel) {
                        if matches!(r.kind, Kind::Color) && !(v.len() == 7 && v.starts_with('#') && v[1..].chars().all(|c| c.is_ascii_hexdigit())) {
                            self.status = ("colours are #RRGGBB".into(), false);
                            return;
                        }
                        let lit = format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""));
                        self.write(r, lit);
                    }
                }
                Key::Esc => self.edit = None,
                k => {
                    inp.handle(k);
                }
            }
            return;
        }
        if let Key::Mouse(btn, x, y, true) = k {
            match btn {
                64 => return self.key(Key::Up),
                65 => return self.key(Key::Down),
                0 if x < 20 => {
                    if y >= 2 && (y - 2) % 2 == 0 && (y - 2) / 2 < PAGES.len() {
                        self.page = (y - 2) / 2;
                        self.sel = 0;
                        self.side = false;
                    }
                }
                0 if y >= 5 => {
                    let i = y - 5;
                    if i < rows(self.page).len() {
                        if self.sel == i && !self.side { self.adjust(1.0) } else { self.sel = i; self.side = false; }
                    }
                }
                _ => {}
            }
            return;
        }
        if self.page_actions(&k) {
            return;
        }
        let n = rows(self.page).len();
        match k {
            Key::Char('q') | Key::Ctrl('c') => self.quit = true,
            Key::Esc if self.side => self.quit = true,
            Key::Esc | Key::Tab | Key::BackTab => self.side = !self.side,
            Key::Up if self.side => { self.page = (self.page + PAGES.len() - 1) % PAGES.len(); self.sel = 0; }
            Key::Down if self.side => { self.page = (self.page + 1) % PAGES.len(); self.sel = 0; }
            Key::Right | Key::Enter if self.side => self.side = false,
            Key::Up => self.sel = self.sel.saturating_sub(1),
            Key::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            Key::Left | Key::Char('-') => self.adjust(-1.0),
            Key::Right | Key::Char('+') | Key::Enter | Key::Char(' ') => self.adjust(1.0),
            _ => {}
        }
    }

    fn draw(&self, t: &mut Term) {
        let u = &self.ui;
        let (w, h) = (t.w, t.h);
        t.clear(u.base);
        if w < 60 || h < 16 {
            t.text(0, 0, "window too small for shadow-config", u.alert, w);
            return;
        }
        let bar = Style::new(u.theme.c("bg"), u.theme.c("accent")).bold();
        t.fill(0, 0, w, 1, bar);
        t.text(1, 0, " SHADOW TUI // RESTRICTED // CONFIG ", bar, w);
        let path = format!(" {} ", crate::common::short(&config_path().to_string_lossy()));
        t.text(w.saturating_sub(path.chars().count() + 1), 0, &path, bar, w);
        let sw = 20;
        t.fill(0, 1, sw, h - 3, u.panel);
        for (i, (name, _)) in PAGES.iter().enumerate() {
            let y = 2 + i * 2;
            if y >= h - 3 {
                break;
            }
            let st = if i == self.page { if self.side { u.sel } else { Style::new(u.theme.c("accent"), u.theme.c("bg_alt")).bold() } } else { Style::new(u.theme.c("fg_dim"), u.theme.c("bg_alt")) };
            t.fill(0, y, sw, 1, st);
            t.text(0, y, &format!("{} {name}", if i == self.page { "▌" } else { " " }), st, sw);
        }
        for y in 1..h - 2 {
            t.put(sw, y, '│', Style::new(u.theme.c("inactive"), u.theme.c("bg")));
        }
        let x0 = sw + 2;
        let mw = w - x0 - 1;
        let (pname, hint) = PAGES[self.page];
        t.text(x0, 2, &pname.to_uppercase(), u.accent, mw);
        t.text(x0, 3, hint, u.dim, mw);
        let rs = rows(self.page);
        for (i, r) in rs.iter().enumerate() {
            let y = 5 + i;
            if y >= h - 3 {
                break;
            }
            let selected = i == self.sel && !self.side;
            let st = if selected { Style::new(u.theme.c("fg"), u.theme.c("surface")).bold() } else { u.base };
            t.fill(x0, y, mw, 1, st);
            if selected {
                t.put(x0, y, '▶', u.accent);
            }
            t.text(x0 + 2, y, r.label, st, mw.saturating_sub(24));
            let v = self.value(r);
            let shown = match r.kind {
                Kind::Bool => if v == "true" { "[x] on".to_string() } else { "[ ] off".to_string() },
                Kind::Choice(_) => format!("◀ {v} ▶"),
                Kind::Num { .. } => format!("◀ {v} ▶"),
                Kind::Color => format!("■ {v}"),
                Kind::Text => if v.is_empty() { "(not set)".into() } else { v.clone() },
            };
            let vx = x0 + mw.saturating_sub(shown.chars().count() + 2);
            let vst = if matches!(r.kind, Kind::Color) { Style::new(crate::term::Color::hex(&v), if selected { u.theme.c("surface") } else { u.theme.c("bg") }) } else { st };
            if selected {
                if let Some(inp) = &self.edit {
                    inp.draw(t, x0 + mw / 2, y, mw / 2 - 1, u.input, true, None);
                    continue;
                }
            }
            t.text(vx, y, &shown, vst, mw);
        }
        // page panels
        let py = 6 + rs.len();
        let panel = match self.page {
            4 => Some((format!("1 auto  2 performance  3 balanced  4 quiet  5 cool  6 low-power   (u refresh)\n\n{}", self.power), "shadow-powerd")),
            6 => Some((self.healer.clone(), "shadow-healer (mode is set by root in /etc/shadow-tui/healer.toml)")),
            8 => Some((self.sec.clone(), "shadow-sec — enable modules with sudo shadow-sec enable MODULE")),
            _ => None,
        };
        if let Some((text, title)) = panel {
            t.text(x0, py, title, u.accent2, mw);
            for (i, l) in text.lines().enumerate() {
                if py + 1 + i >= h - 3 {
                    break;
                }
                t.text(x0, py + 1 + i, l, u.dim, mw);
            }
        }
        let st = if self.status.1 { Style::new(u.theme.c("ok"), u.theme.c("bg_alt")) } else { Style::new(u.theme.c("alert"), u.theme.c("bg_alt")).bold() };
        t.fill(0, h - 2, w, 1, st);
        t.text(1, h - 2, &self.status.0, st, w - 2);
        t.text(1, h - 1, if self.side { "↑↓ page · → open · q quit" } else { "↑↓ select · ←→ adjust · Enter edit/toggle · Tab sidebar · mouse: click/wheel · q quit" }, u.dim, w - 2);
    }
}

pub fn run(start: Option<&str>) -> i32 {
    let text = std::fs::read_to_string(config_path()).unwrap_or_default();
    let mut app = App {
        ui: Ui::new(&crate::common::active_theme_tactical(), false),
        page: start.and_then(|s| PAGES.iter().position(|(n, _)| n.to_lowercase().starts_with(&s.to_lowercase()))).unwrap_or(0),
        sel: 0, side: false, text, edit: None, status: ("changes apply live through shadowctl; a rejected change is rolled back".into(), true),
        quit: false, power: String::new(), sec: String::new(), healer: String::new(),
    };
    let _ = find_theme;
    app.refresh();
    let mut t = match Term::enter(None) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("shadow-config: {e}");
            return 1;
        }
    };
    t.mouse(true);
    while !app.quit {
        if t.resize() {
            t.force_redraw();
        }
        app.draw(&mut t);
        let _ = t.present();
        for k in t.keys() {
            app.key(k);
        }
    }
    t.leave();
    0
}

#[cfg(test)]
mod t {
    #[test]
    fn toml_edit_keeps_comments() {
        let src = "# top\n[layout]\n# height = 84  # comment\n\n[look]\nblur = true\n";
        let a = super::set_in(src, "layout", "height", "90");
        assert_eq!(super::get_in(&a, "layout", "height").as_deref(), Some("90"));
        assert!(a.starts_with("# top\n[layout]\nheight = 90\n"));
        let b = super::set_in(&a, "look", "blur", "false");
        assert_eq!(super::get_in(&b, "look", "blur").as_deref(), Some("false"));
        let c = super::set_in(&b, "hud", "height_px", "30");
        assert!(c.ends_with("[hud]\nheight_px = 30\n"));
        assert_eq!(super::get_in(&c, "look", "amber"), None);
    }
}
