//! shadow-config — live settings matrix for ~/.config/shadow-state/config.toml.
//!
//! Every change is written at once and applied live: layout, look and input regenerate niri's
//! config (validated by niri first — a rejected change is rolled back); display and session
//! settings are picked up by shadow-displayd; the side bar restarts when its look changes.
//!
//!   ↑↓ select · ←→ adjust · Enter edit / toggle · a scale back to auto · Tab / 1-9 page · Esc close
use shadow_apps::cfg::{get_in, remove_in, set_in};
use shadow_apps::{atomic_write, cmd_out, config_path, spawn};
use shadow_ui::widgets::{chrome, footer, selection, Input};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Num { min: f64, max: f64, step: f64, float: bool },
    Bool,
    Choice(&'static [&'static str]),
    Text,
    Color,
    /// comma-separated percentages stored as a TOML array
    Percents,
    /// a per-monitor scale: absent = automatic
    Scale,
}

#[derive(Clone)]
struct Row {
    section: String,
    key: String,
    label: String,
    kind: Kind,
    default: &'static str,
}

fn r(section: &str, key: &str, label: &str, kind: Kind, default: &'static str) -> Row {
    Row { section: section.into(), key: key.into(), label: label.into(), kind, default }
}
const fn num(min: f64, max: f64, step: f64, float: bool) -> Kind {
    Kind::Num { min, max, step, float }
}

const PAGES: [(&str, &str); 9] = [
    ("Center stage", "width presets, default width, gaps, transition time"),
    ("Look", "colours, opacity, font and text size"),
    ("Displays", "per-monitor scale and the AC / battery refresh rates"),
    ("Side bar", "the floating information bar"),
    ("Input", "keyboard layout and repeat"),
    ("Session", "idle lock and monitor power-off"),
    ("Power + NPU", "learning schedule, predictive tuning, modes"),
    ("Updates + sync", "update source + its policy (set up: shadow update setup) and the GitHub state repository"),
    ("Security", "optional shadow-sec: what this OS supports · S opens the security console to enable modules"),
];

fn rows(page: usize, outputs: &[String]) -> Vec<Row> {
    match page {
        0 => vec![
            r("layout", "presets", "Width presets (% of the monitor)", Kind::Percents, "45, 65, 85"),
            r("layout", "default_preset", "Default width preset (0 = narrowest)", num(0.0, 2.0, 1.0, false), "1"),
            r("layout", "gaps", "Gaps (px)", num(0.0, 48.0, 2.0, false), "12"),
            r("layout", "anim_ms", "Transition time (ms, cubic-bezier)", num(0.0, 600.0, 20.0, false), "120"),
        ],
        1 => vec![
            r("look", "amber", "Tactical amber", Kind::Color, "#FFB000"),
            r("look", "cyan", "Cyan", Kind::Color, "#00E5FF"),
            r("look", "crimson", "Crimson", Kind::Color, "#FF2A00"),
            r("look", "obsidian", "Obsidian", Kind::Color, "#0A0B0E"),
            r("look", "opacity", "Panel opacity", num(0.3, 1.0, 0.05, true), "0.92"),
            r("look", "font", "Font family", Kind::Text, "JetBrainsMono Nerd Font Mono"),
            r("look", "font_pt", "Text size (pt, before scaling)", num(8.0, 20.0, 0.5, true), "11"),
        ],
        2 => {
            let mut v: Vec<Row> = outputs.iter().map(|o| r("display.scale", &format!("\"{o}\""), &format!("Scale of {o}"), Kind::Scale, "auto")).collect();
            v.push(r("display", "refresh_policy", "Panel refresh policy", Kind::Choice(&["auto", "ac", "battery", "fixed"]), "auto"));
            v.push(r("display", "refresh_ac", "Refresh on AC (Hz)", num(48.0, 240.0, 12.0, false), "120"));
            v.push(r("display", "refresh_battery", "Refresh on battery (Hz)", num(30.0, 120.0, 12.0, false), "60"));
            v
        }
        3 => vec![r("bar", "width", "Bar width (px)", num(180.0, 480.0, 10.0, false), "250")],
        4 => vec![
            r("input", "xkb_layout", "Keyboard layout (e.g. us,ca)", Kind::Text, ""),
            r("input", "xkb_options", "XKB options (e.g. ctrl:nocaps)", Kind::Text, ""),
            r("input", "repeat_delay", "Key repeat delay (ms)", num(150.0, 800.0, 25.0, false), "300"),
            r("input", "repeat_rate", "Key repeat rate (/s)", num(10.0, 80.0, 5.0, false), "40"),
        ],
        5 => vec![
            r("session", "lock_after_min", "Lock after idle (minutes, 0 = never)", num(0.0, 120.0, 1.0, false), "10"),
            r("session", "screen_off_after_min", "Monitors off after idle (minutes, 0 = never)", num(0.0, 180.0, 1.0, false), "12"),
        ],
        6 => vec![r("power", "learning", "Learn and predict (shadow-powerd)", Kind::Bool, "true")],
        7 => vec![
            r("updates", "repo", "Update source (git URL)", Kind::Text, ""),
            r("updates", "branch", "Branch", Kind::Text, "main"),
            r("updates", "auth", "Authentication (deploy key = read-only SSH key)", Kind::Choice(&["deploy-key", "credential-helper"]), "deploy-key"),
            r("updates", "require_private", "Require a private repository", Kind::Bool, "true"),
            r("updates", "require_signed", "Require signed commits (update-allowed-signers)", Kind::Bool, "false"),
            r("updates", "allow_history_rewrite", "Allow force-pushed history", Kind::Bool, "false"),
            r("sync", "age_recipient", "secrets.age recipient (empty = ~/.ssh/id_ed25519.pub)", Kind::Text, ""),
        ],
        _ => vec![],
    }
}

struct Config {
    text: String,
    page: usize,
    sel: usize,
    outputs: Vec<String>,
    edit: Option<Input>,
    status: (String, bool),
    info: Vec<String>,
}

fn unq(k: &str) -> &str {
    k.trim_matches('"')
}

impl Config {
    fn rows(&self) -> Vec<Row> {
        rows(self.page, &self.outputs)
    }

    fn value(&self, r: &Row) -> String {
        get_in(&self.text, &r.section, unq(&r.key)).unwrap_or_else(|| r.default.to_string())
    }

    fn shown(&self, r: &Row) -> String {
        let v = self.value(r);
        match r.kind {
            Kind::Percents => v.trim_matches(|c| c == '[' || c == ']').split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).map(|x| x.parse::<f64>().map(|f| if f <= 1.0 { format!("{}", (f * 100.0).round()) } else { format!("{}", f.round()) }).unwrap_or_else(|_| x.to_string())).collect::<Vec<_>>().join(", "),
            Kind::Bool => if v == "true" { "on".into() } else { "off".into() },
            Kind::Scale if v == "auto" => {
                let st = cmd_out(&["shadow-displayd", "status"]).1;
                let name = unq(&r.key);
                let ideal = st.lines().find(|l| l.starts_with(&format!("{name} "))).and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix("ideal="))).unwrap_or("?").to_string();
                format!("auto ({ideal})")
            }
            _ => v,
        }
    }

    fn refresh_info(&mut self) {
        self.info = match self.page {
            2 => cmd_out(&["shadow-displayd", "status"]).1.lines().map(String::from).collect(),
            6 => {
                let mut v: Vec<String> = cmd_out(&["shadow-powerd", "learn"]).1.lines().take(14).map(String::from).collect();
                v.push(String::new());
                v.push("R relearn now (fresh week) · 1 auto 2 performance 3 balanced 4 quiet 5 cool 6 low-power".into());
                v
            }
            8 => {
                let (c, o) = cmd_out(&["shadow-sec", "precheck", "--porcelain"]);
                if c == 127 {
                    vec!["shadow-sec is not installed (optional package shadow-tui-sec: ./install.sh --sec)".into()]
                } else {
                    // one line per module: state and whether this OS supports it
                    let mut v: Vec<String> = o
                        .lines()
                        .filter_map(|l| {
                            let f: Vec<&str> = l.split_whitespace().collect();
                            match f.as_slice() {
                                [m, "supported", st] => Some(format!("{m:<8} {:<9} can be enabled here", st)),
                                [m, "unsupported", st] => Some(format!("{m:<8} {:<9} not supported on this system — {}", st, o.lines().find(|x| x.starts_with(&format!("{m} fail "))).map(|x| x.splitn(3, ' ').nth(2).unwrap_or("").split(" | ").next().unwrap_or("")).unwrap_or(""))),
                                _ => None,
                            }
                        })
                        .collect();
                    v.push(String::new());
                    v.push("S  open the security console: details, enable / disable (asks for your password each time)".into());
                    v
                }
            }
            _ => vec![],
        };
    }

    /// Apply a change: write, then make it live; roll back if niri rejects the generated config.
    fn write(&mut self, r: &Row, literal: Option<String>) {
        let before = self.text.clone();
        let after = match &literal {
            Some(l) => set_in(&self.text, &r.section, &r.key, l),
            None => remove_in(&self.text, &r.section, unq(&r.key)),
        };
        if atomic_write(&config_path(), &after).is_err() {
            self.status = (format!("cannot write {}", config_path().display()), true);
            return;
        }
        self.text = after;
        let shown = literal.clone().unwrap_or_else(|| "auto".into());
        match r.section.as_str() {
            "layout" | "look" | "input" => {
                let (code, out) = cmd_out(&["shadow", "setup", "niri"]);
                if code == 0 || code == 127 {
                    self.status = (format!("{} = {shown} · applied live", unq(&r.key)), false);
                } else {
                    let _ = atomic_write(&config_path(), &before);
                    self.text = before;
                    let _ = cmd_out(&["shadow", "setup", "niri"]);
                    self.status = (format!("rejected, rolled back: {}", out.lines().last().unwrap_or("").trim()), true);
                    return;
                }
                if r.section == "look" {
                    spawn("sh -c 'pkill -x shadow-bar; sleep 0.3; exec shadow-bar'");
                }
            }
            "bar" => {
                spawn("sh -c 'pkill -x shadow-bar; sleep 0.3; exec shadow-bar'");
                self.status = (format!("{} = {shown} · bar restarted", unq(&r.key)), false);
            }
            "power" => {
                let _ = cmd_out(&["shadow-powerd", "learn", if literal.as_deref() == Some("true") { "--on" } else { "--off" }]);
                self.status = (format!("learning {}", if literal.as_deref() == Some("true") { "on" } else { "off" }), false);
            }
            _ => self.status = (format!("{} = {shown} · applied", unq(&r.key)), false),
        }
        self.refresh_info();
    }

    fn adjust(&mut self, dir: f64) {
        let rs = self.rows();
        let Some(r) = rs.get(self.sel).cloned() else { return };
        let v = self.value(&r);
        let lit = match r.kind {
            Kind::Num { min, max, step, float } => {
                let x = (v.parse::<f64>().unwrap_or(min) + dir * step).clamp(min, max);
                if float { format!("{:.2}", x).trim_end_matches('0').trim_end_matches('.').to_string() } else { format!("{}", x.round() as i64) }
            }
            Kind::Scale => {
                let cur = v.parse::<f64>().unwrap_or(1.5);
                let x = if v == "auto" { cur } else { (cur + dir * 0.25).clamp(1.0, 3.0) };
                format!("{x:.2}").trim_end_matches('0').trim_end_matches('.').to_string()
            }
            Kind::Bool => (v != "true").to_string(),
            Kind::Choice(opts) => {
                let i = opts.iter().position(|o| *o == v).unwrap_or(0) as i64;
                format!("\"{}\"", opts[((i + dir as i64).rem_euclid(opts.len() as i64)) as usize])
            }
            Kind::Text | Kind::Color | Kind::Percents => {
                let mut i = Input::default();
                i.set(&self.shown(&r));
                self.edit = Some(i);
                return;
            }
        };
        self.write(&r, Some(lit));
    }

    fn commit_edit(&mut self, text: String) {
        let rs = self.rows();
        let Some(r) = rs.get(self.sel).cloned() else { return };
        let lit = match r.kind {
            Kind::Color => match shadow_ui::theme::hex(&text) {
                Some(_) => format!("\"#{}\"", text.trim().trim_start_matches('#').to_uppercase()),
                None => {
                    self.status = ("colour must be #RRGGBB".into(), true);
                    return;
                }
            },
            Kind::Percents => {
                let v: Vec<f64> = text.split(|c| c == ',' || c == ' ').filter(|x| !x.is_empty()).filter_map(|x| x.trim_end_matches('%').parse().ok()).filter(|x: &f64| (10.0..=100.0).contains(x)).collect();
                if v.is_empty() || v.len() > 6 {
                    self.status = ("give 1-6 percentages between 10 and 100, e.g. 45, 65, 85".into(), true);
                    return;
                }
                format!("[{}]", v.iter().map(|x| format!("{x}")).collect::<Vec<_>>().join(", "))
            }
            _ => format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"")),
        };
        self.write(&r, Some(lit));
    }
}

impl App for Config {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-config".into(), title: "Settings".into(), size: (1000, 620) }
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        ctx.redraw();
        if let Some(mut inp) = self.edit.take() {
            match k.sym {
                Keysym::Escape => self.status = ("cancelled".into(), false),
                Keysym::Return | Keysym::KP_Enter => self.commit_edit(inp.text.clone()),
                _ => {
                    inp.key(k);
                    self.edit = Some(inp);
                }
            }
            return;
        }
        let n = self.rows().len();
        let typed = k.typed().unwrap_or("").to_string();
        match (k.sym, typed.as_str()) {
            (Keysym::Escape, _) | (_, "q") => ctx.quit(),
            (Keysym::Up, _) | (_, "k") => self.sel = self.sel.saturating_sub(1),
            (Keysym::Down, _) | (_, "j") => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            (Keysym::Left, _) | (_, "h") => self.adjust(-1.0),
            (Keysym::Right, _) | (_, "l") => self.adjust(1.0),
            (Keysym::Return | Keysym::KP_Enter, _) | (_, " ") => self.adjust(1.0),
            (Keysym::Tab | Keysym::Page_Down, _) => {
                self.page = (self.page + 1) % PAGES.len();
                self.sel = 0;
                self.refresh_info();
            }
            (Keysym::ISO_Left_Tab | Keysym::Page_Up, _) => {
                self.page = (self.page + PAGES.len() - 1) % PAGES.len();
                self.sel = 0;
                self.refresh_info();
            }
            (_, "a") => {
                if let Some(r) = self.rows().get(self.sel).cloned().filter(|r| r.kind == Kind::Scale) {
                    self.write(&r, None);
                }
            }
            (_, "S") | (_, "s") if self.page == 8 => {
                shadow_apps::spawn("shadow-term --hold -e shadow-sec console");
                self.status = ("security console opened in a terminal".into(), false);
            }
            (_, "R") if self.page == 6 => {
                let (_, o) = cmd_out(&["shadow-powerd", "learn", "--relearn"]);
                self.status = (o.lines().last().unwrap_or("").to_string(), false);
                self.refresh_info();
            }
            (_, d) if self.page == 6 && d.len() == 1 && ("1"..="6").contains(&d) => {
                let mode = ["auto", "performance", "balanced", "quiet", "cool", "low-power"][(d.as_bytes()[0] - b'1') as usize];
                let (_, o) = cmd_out(&["shadow-powerd", "mode", mode]);
                self.status = (o.lines().last().unwrap_or("").to_string(), false);
            }
            (_, d) if d.len() == 1 && ("1"..="9").contains(&d) => {
                self.page = (d.as_bytes()[0] - b'1') as usize;
                self.sel = 0;
                self.refresh_info();
            }
            _ => {}
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let (top, pad) = chrome(f, "CONFIG", PAGES[self.page].1);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt;
        let spt = pt * 0.9;
        let lh = f.line_h(pt);
        // page list
        let tab_w = f.px(190.0);
        for (i, (name, _)) in PAGES.iter().enumerate() {
            let y = top + i as i32 * (lh + f.px(6.0));
            let cur = i == self.page;
            if cur {
                f.cv.fill(pad, y, tab_w, lh + f.px(6.0), th.amber, 1.0);
            }
            f.text(pad + f.px(8.0), y + f.px(3.0), &format!("{} {name}", i + 1), pt, if cur { th.bg } else { th.dim }, cur);
        }
        let x = pad + tab_w + f.px(22.0);
        let sep_h = h - top - pad - f.line_h(spt) - f.px(10.0);
        let (sx, hair) = (x - f.px(11.0), f.hair());
        f.cv.fill(sx, top, hair, sep_h, th.amber, 0.35);
        let rw = w - pad - x;
        let rs = self.rows();
        self.sel = self.sel.min(rs.len().saturating_sub(1));
        let row_h = lh + f.px(8.0);
        let mut y = top;
        if rs.is_empty() && self.info.is_empty() {
            f.text(x, y, "nothing to set on this page", pt, th.dim, false);
        }
        for (i, r) in rs.iter().enumerate() {
            let sel = i == self.sel;
            if sel {
                selection(f, x - f.px(6.0), y, rw + f.px(6.0), row_h);
            }
            f.text(x + f.px(4.0), y + f.px(4.0), &r.label, pt, th.text, false);
            let val = if sel && self.edit.is_some() { String::new() } else { self.shown(r) };
            if sel {
                if let Some(inp) = &self.edit {
                    let ix = x + rw * 55 / 100;
                    f.cv.fill(ix - f.px(4.0), y + f.px(2.0), rw - (ix - x) - f.px(4.0), row_h - f.px(4.0), th.bg, 1.0);
                    inp.draw(f, ix, y + f.px(4.0), pt, true);
                }
            }
            if !val.is_empty() {
                let vw = f.text_w(&val, pt, false);
                let vx = x + rw - vw - f.px(8.0);
                if r.kind == Kind::Color {
                    if let Some(c) = shadow_ui::theme::hex(&val) {
                        let s = lh - f.px(4.0);
                        f.cv.fill(vx - s - f.px(8.0), y + f.px(6.0), s, s, c, 1.0);
                    }
                }
                f.text(vx, y + f.px(4.0), &val, pt, th.cyan, sel);
            }
            y += row_h;
        }
        let slh = f.line_h(spt);
        y += f.px(10.0);
        for l in &self.info {
            if y + slh > h - pad - slh * 3 {
                break;
            }
            let t = f.fit(l, spt, false, rw);
            f.text(x, y, &t, spt, th.dim, false);
            y += slh;
        }
        if !self.status.0.is_empty() {
            let sy = h - pad - f.line_h(spt) * 2 - f.px(14.0);
            let s = f.fit(&self.status.0, pt, false, rw);
            f.text(x, sy, &s, pt, if self.status.1 { th.crimson } else { th.green }, false);
        }
        footer(f, "↑↓ select · ←→ adjust · Enter edit/toggle · a auto scale · Tab or 1-9 page · Esc close");
    }
}

fn main() {
    let outputs = shadow_niri::outputs().map(|v| v.into_iter().map(|o| o.name).collect()).unwrap_or_default();
    let start = std::env::args().nth(1).and_then(|a| PAGES.iter().position(|(n, _)| n.to_lowercase().starts_with(&a.to_lowercase()))).unwrap_or(0);
    let mut c = Config { text: std::fs::read_to_string(config_path()).unwrap_or_default(), page: start, sel: 0, outputs, edit: None, status: (String::new(), false), info: vec![] };
    c.refresh_info();
    if let Err(e) = run(Box::new(c), None) {
        eprintln!("shadow-config: {e}");
        std::process::exit(1);
    }
}
