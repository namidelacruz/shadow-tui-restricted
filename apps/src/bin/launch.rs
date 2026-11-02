//! shadow-launch — the application launcher. It opens as its own normal window (tiled by niri,
//! fully opaque), not as an overlay over what is on screen.
//!
//! Type to search apps and Shadow actions. With an empty query the list is ranked by intent:
//! what usually follows the app you're in (Markov transitions), what you open at this hour, and
//! how often. Each app shows what it cost last time: ~RAM (measured by shadow-sessiond) and
//! ~W (from the CPU share shadow-powerd learned for it).
//!
//!   text   search       !cmd  run a command      >  Shadow actions only
//!   ↑ ↓    select       Enter launch             Esc close        click a row to launch it
use shadow_apps::desktop::{self, Entry};
use shadow_apps::{focused_app, spawn};
use shadow_ui::widgets::{chrome_solid, field, footer, selection, Input, ListNav};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};
use std::collections::HashMap;

struct Launch {
    all: Vec<Entry>,
    shown: Vec<(usize, i32)>,
    input: Input,
    nav: ListNav,
    visible: usize,
    from: String,
    hist: HashMap<String, i32>,
    hours: HashMap<String, [u32; 24]>,
    markov: HashMap<(String, String), u32>,
    ram: HashMap<String, u32>,
    cpu: HashMap<u32, f64>,
    hour: usize,
    /// geometry of the drawn list, for clicks: (top, row height)
    rows: (i32, i32),
}

fn action(id: &str, name: &str, detail: &str, cmd: &str) -> Entry {
    Entry { id: format!("shadow:{id}"), name: name.into(), detail: detail.into(), tag: "shadow", cmd: cmd.into(), terminal: false, keywords: "shadow".into(), cats: "Shadow".into(), app_ids: vec![] }
}

fn hold(cmd: &str) -> String {
    format!("shadow-term --hold -e sh -c {}", shadow_apps::sh_quote(cmd))
}

fn actions() -> Vec<Entry> {
    let mut v = vec![
        action("term", "Terminal", "shadow-term", "shadow-term"),
        action("net", "Network", "shadow-net · Wi-Fi, campus profiles, repair", "shadow-net"),
        action("config", "Settings", "shadow-config · live settings matrix", "shadow-config"),
        action("files", "Files", "shadow-files", "shadow-files"),
        action("lock", "Lock screen", "", "shadow-lock"),
        action("prefs", "Default apps", "shadow-prefs · file manager, terminal, background", "shadow-prefs"),
        action("crash", "Crash log", "what crashed, what holds changes back · save a report", "shadow-prefs crash"),
        action("hub", "Hub", "Mod+Space · apps, settings, power, system, look", "shadow-hub summon"),
        action("bar", "Show / hide side bar", "Mod+Shift+B · optional next to the hub", "sh -c 'pgrep -x shadow-bar >/dev/null && pkill -USR1 -x shadow-bar || exec shadow-bar'"),
        action("overview", "Overview", "all workspaces", "niri msg action toggle-overview"),
        action("power", "Power + learning", "shadow-powerd status and schedule", &hold("shadow-powerd status; echo; shadow-powerd learn")),
        action("sync", "Sync state to GitHub", "shadow-sync push", &hold("shadow-sync push")),
        action("healer", "Healer status", "self-correction log and proposals", &hold("shadow healer status")),
        action("revert", "Revert last automated fix", "Mod+Shift+Backspace", "shadow healer revert"),
        action("fix", "Fix center", "shadow-fix · app problems, tasks, login & passwords, update + triage", "shadow-fix"),
        action("passfix", "Password repair", "lockouts, keyboard layout, PAM, keyring — test your password safely", "shadow-term --hold -e shadow-passfix"),
        action("wallpaper", "Wallpaper", "shadow-wallpaper · choose, per monitor, fill / fit", "shadow-wallpaper pick"),
        action("tasks", "Task manager", "shadow-tasks · CPU, RAM, NPU, GPU, processes", "shadow-tasks"),
        action("update", "Update", "shadow-update · check for and install updates (only changed files)", "shadow-update"),
        action("display", "Display status", "scale, text size, refresh per monitor", &hold("shadow-displayd status")),
        action("logout", "Log out", "leave the session", "niri msg action quit --skip-confirmation"),
        action("suspend", "Suspend", "", "systemctl suspend"),
        action("reboot", "Reboot", "", "systemctl reboot"),
        action("poweroff", "Shut down", "", "systemctl poweroff"),
    ];
    if shadow_apps::cmd_out(&["sh", "-c", "command -v shadow-sec"]).0 == 0 {
        v.push(action("sec", "Security console", "shadow-sec · precheck, enable modules (asks for your password)", "shadow-term --hold -e shadow-sec console"));
        v.push(action("secreport", "Security report", "shadow-sec control mapping (read-only)", &hold("shadow-sec report")));
    }
    v
}

impl Launch {
    fn intent(&self, e: &Entry) -> i32 {
        let h = self.hist.get(&e.id).copied().unwrap_or(0).min(50);
        let hr = desktop::hour_score(&self.hours, &e.id, self.hour).min(40);
        let mk = desktop::markov_score(&self.markov, &self.from, &e.id);
        // before there is any history, the core Shadow apps lead the list
        let base = match e.id.as_str() {
            "shadow:term" => 4,
            "shadow:net" | "shadow:files" | "shadow:tasks" => 3,
            "shadow:config" => 2,
            _ => 0,
        };
        3 * mk + 2 * hr + h + base
    }

    fn filter(&mut self) {
        let q = self.input.text.trim().to_string();
        let mut v: Vec<(usize, i32)> = vec![];
        if let Some(cmd) = q.strip_prefix('!') {
            if !cmd.trim().is_empty() {
                v.push((usize::MAX, 0));
            }
        } else {
            let (only_actions, q) = match q.strip_prefix('>') {
                Some(r) => (true, r.trim().to_string()),
                None => (false, q),
            };
            for (i, e) in self.all.iter().enumerate() {
                if only_actions && e.tag != "shadow" {
                    continue;
                }
                let hay = format!("{} {} {}", e.name, e.keywords, e.detail);
                if let Some(s) = desktop::score(&q, &hay) {
                    let rank = if q.is_empty() { self.intent(e) } else { s * 4 + desktop::name_bonus(&q, &e.name) + self.intent(e) / 4 };
                    v.push((i, rank));
                }
            }
            v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| self.all[a.0].name.to_lowercase().cmp(&self.all[b.0].name.to_lowercase())));
        }
        self.shown = v;
        self.nav.sel = 0;
        self.nav.clamp(self.shown.len(), self.visible);
    }

    fn launch(&mut self, ctx: &mut Ctx) {
        let q = self.input.text.trim().to_string();
        if let Some(cmd) = q.strip_prefix('!') {
            let cmd = cmd.trim();
            if let Some(c) = cmd.strip_prefix('!') {
                spawn(&hold(c.trim()));
            } else {
                spawn(cmd);
            }
            ctx.quit();
            return;
        }
        let Some(&(i, _)) = self.shown.get(self.nav.sel) else { return };
        let e = self.all[i].clone();
        desktop::record_launch(&e.id, &self.from);
        if e.terminal {
            spawn(&shadow_apps::terminal_exec(&e.cmd));
        } else {
            spawn(&e.cmd);
        }
        ctx.quit();
    }
}

impl App for Launch {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-launch".into(), title: "Launch".into(), size: (680, 460) }
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        match k.sym {
            Keysym::Escape => ctx.quit(),
            Keysym::Return | Keysym::KP_Enter => self.launch(ctx),
            Keysym::Up | Keysym::Down | Keysym::Page_Up | Keysym::Page_Down => {
                self.nav.key(k, self.shown.len(), self.visible, false);
                ctx.redraw();
            }
            Keysym::Tab => {
                self.nav.sel = (self.nav.sel + 1) % self.shown.len().max(1);
                self.nav.clamp(self.shown.len(), self.visible);
                ctx.redraw();
            }
            _ => {
                if self.input.key(k) {
                    self.filter();
                    ctx.redraw();
                }
            }
        }
    }

    fn click(&mut self, _x: i32, y: i32, ctx: &mut Ctx) {
        let (top, rh) = self.rows;
        if rh <= 0 || y < top {
            return;
        }
        let row = self.nav.top + ((y - top) / rh) as usize;
        if row < self.shown.len() && row < self.nav.top + self.visible {
            self.nav.sel = row;
            self.launch(ctx);
        }
    }

    fn scroll(&mut self, lines: i32, ctx: &mut Ctx) {
        let n = self.shown.len();
        if n == 0 {
            return;
        }
        self.nav.sel = (self.nav.sel as i64 + lines as i64).clamp(0, n as i64 - 1) as usize;
        self.nav.clamp(n, self.visible);
        ctx.redraw();
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let ctxline = if self.from.is_empty() { String::new() } else { format!("after {}", self.from) };
        let (top, pad) = chrome_solid(f, "LAUNCH", &ctxline);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt;
        let lh = f.line_h(pt);
        // query line
        let qy = top;
        field(f, pad, qy, w - 2 * pad, lh + f.px(10.0), true);
        let px = pad + f.px(8.0);
        let pw = f.text(px, qy + f.px(5.0), "▸ ", pt, th.amber, true);
        if self.input.text.is_empty() {
            let hint = "search apps   !command   >actions";
            f.text_a(px + pw, qy + f.px(5.0), hint, pt, th.dim, false, 0.8);
            f.cv.fill(px + pw, qy + f.px(5.0), f.hair().max(f.px(2.0)), lh, th.amber, 1.0);
        } else {
            self.input.draw(f, px + pw, qy + f.px(5.0), pt, true);
        }
        // list
        let row_h = lh + f.px(8.0);
        let list_top = qy + lh + f.px(22.0);
        self.rows = (list_top, row_h);
        let foot = f.line_h(pt * 0.9) + pad + f.px(6.0);
        self.visible = ((h - list_top - foot) / row_h).max(1) as usize;
        self.nav.clamp(self.shown.len(), self.visible);
        if self.shown.is_empty() {
            f.text(pad + f.px(8.0), list_top, "no match", pt, th.dim, false);
        }
        for (row, &(i, _)) in self.shown.iter().enumerate().skip(self.nav.top).take(self.visible) {
            let y = list_top + (row - self.nav.top) as i32 * row_h;
            let sel = row == self.nav.sel;
            if sel {
                selection(f, pad, y, w - 2 * pad, row_h);
            }
            let ty = y + f.px(4.0);
            let x0 = pad + f.px(17.0);
            if i == usize::MAX {
                let cmd = self.input.text.trim().trim_start_matches('!').to_string();
                f.text(x0, ty, &format!("run  {cmd}"), pt, th.cyan, sel);
                continue;
            }
            let e = &self.all[i];
            let (ram, watts) = desktop::estimate(e, &self.ram, &self.cpu);
            let est = match (ram, watts) {
                (Some(r), Some(w)) => format!("~{r} MB · ~{w:.0} W"),
                (Some(r), None) => format!("~{r} MB"),
                (None, Some(w)) => format!("~{w:.0} W"),
                _ => String::new(),
            };
            let est_w = if est.is_empty() { 0 } else { f.text_w(&est, pt * 0.9, false) };
            let name_col = if e.tag == "shadow" { th.amber } else { th.text };
            let nw = f.text(x0, ty, &e.name, pt, name_col, sel);
            if !e.detail.is_empty() {
                let max = w - pad - x0 - nw - est_w - f.px(40.0);
                if max > f.px(30.0) {
                    let d = f.fit(&e.detail, pt * 0.9, false, max);
                    let dy = ty + (lh - f.line_h(pt * 0.9)) / 2;
                    f.text(x0 + nw + f.px(14.0), dy, &d, pt * 0.9, th.dim, false);
                }
            }
            if !est.is_empty() {
                let dy = ty + (lh - f.line_h(pt * 0.9)) / 2;
                f.text(w - pad - f.px(10.0) - est_w, dy, &est, pt * 0.9, th.cyan, false);
            }
        }
        footer(f, "type to search · ↑↓ / wheel select · Enter or click launch · !cmd run · >actions · Esc close");
    }
}

fn main() {
    let mut all = actions();
    all.extend(desktop::load_apps());
    let mut l = Launch {
        all,
        shown: vec![],
        input: Input::default(),
        nav: ListNav::default(),
        visible: 10,
        from: focused_app(),
        hist: desktop::load_history(),
        hours: desktop::load_hours(),
        markov: desktop::load_markov(),
        ram: desktop::load_ram(),
        cpu: desktop::load_cpu(),
        hour: shadow_apps::local_hour(),
        rows: (0, 0),
    };
    if l.from == "shadow-launch" {
        l.from.clear();
    }
    l.filter();
    if let Err(e) = run(Box::new(l), None) {
        eprintln!("shadow-launch: {e}");
        std::process::exit(1);
    }
}
