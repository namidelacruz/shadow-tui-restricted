//! shadow-fix — the Fix center window (Mod+Shift+H). Drives `shadow fix …` and shadow-passfix.
//!
//!   ISSUES    known problems of your apps under niri, with repairs (Enter / click). Repairs that need
//!             administrator rights open a terminal, where sudo asks for the password.
//!   TASKS     everyday "it's stuck" tasks: clear an app's caches, unlock a Firefox profile, force quit,
//!             restart the portals / PipeWire
//!   LOGIN     password and login problems (lockouts, keyboard layout, PAM, keyring): the checks, and
//!             the repair console (also on a text console: shadow-passfix)
//!   UPDATE    update the system and Flatpaks in a console, then triage what the updates changed —
//!             repairs happen after the update, never during it
//!   DESKTOP   the solid background colour (on / off), the hub (on / side bar instead), the hub check,
//!             the crash log and the default apps
//! Tab switches page · ↑↓ select · Enter apply · r rescan · Esc close
use shadow_ui::calloop::channel::{self, Sender};
use shadow_ui::widgets::{chrome, footer, selection, Rect};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};

const PAGES: [&str; 5] = ["ISSUES", "TASKS", "LOGIN", "UPDATE", "DESKTOP"];

#[derive(Clone)]
enum Act {
    Fix(String, bool),
    Task(String),
    Term(String),
    /// start a program (no terminal)
    Exec(String),
    /// flip a [section] key between true and false, then rewrite niri's config
    Toggle(&'static str, &'static str),
    /// run in the background and show the last line
    Work(Vec<String>),
    None,
}

fn conf_bool(section: &str, key: &str, default: bool) -> bool {
    let t = std::fs::read_to_string(shadow_apps::config_path()).unwrap_or_default();
    shadow_apps::cfg::get_in(&t, section, key).map(|v| v.trim_matches('"').trim() != "false").unwrap_or(default)
}

#[derive(Clone)]
struct Row {
    lvl: char, // '!' issue, '✓' ok, '·' info, '✗' bad
    text: String,
    detail: String,
    act: Act,
}

struct Fixc {
    page: usize,
    rows: Vec<Row>,
    sel: usize,
    top: usize,
    visible: usize,
    busy: bool,
    msg: (String, bool),
    tx: Sender<String>,
    hits: Vec<(usize, Rect)>,
    tabs: Vec<(usize, Rect)>,
}

fn sh(args: &[&str]) -> (i32, String) {
    match std::process::Command::new(args[0]).args(&args[1..]).stdin(std::process::Stdio::null()).output() {
        Ok(o) => (o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)),
        Err(_) => (127, format!("{} not found", args[0])),
    }
}

fn term(cmd: &str) {
    shadow_apps::spawn(&format!("shadow-term --hold -e sh -c {}", shadow_apps::sh_quote(cmd)));
}

impl Fixc {
    fn load(&mut self) {
        self.rows = match PAGES[self.page] {
            "ISSUES" => {
                let (_, o) = sh(&["shadow", "fix", "scan", "--porcelain"]);
                let mut v: Vec<Row> = o
                    .lines()
                    .filter_map(|l| {
                        let (head, detail) = l.split_once('|')?;
                        let mut w = head.splitn(5, ' ');
                        let (kind, id, root, _app, title) = (w.next()?, w.next()?, w.next()? == "1", w.next()?, w.next()?);
                        Some(Row {
                            lvl: if kind == "issue" { '!' } else { '✓' },
                            text: title.to_string(),
                            detail: format!("{detail}{}", if kind == "issue" && root { " · needs your password" } else { "" }),
                            act: if kind == "issue" { Act::Fix(id.to_string(), root) } else { Act::None },
                        })
                    })
                    .collect();
                // problems first
                v.sort_by_key(|r| r.lvl != '!');
                if v.is_empty() {
                    v.push(Row { lvl: '·', text: "nothing detected".into(), detail: String::new(), act: Act::None });
                }
                v
            }
            "TASKS" => {
                let (_, o) = sh(&["shadow", "fix", "tasks", "--porcelain"]);
                o.lines()
                    .filter_map(|l| {
                        let (id, label) = l.split_once('|')?;
                        let (name, what) = label.split_once(": ").unwrap_or(("", label));
                        Some(Row { lvl: '·', text: if name.is_empty() { what.to_string() } else { format!("{name}: {what}") }, detail: id.to_string(), act: Act::Task(id.to_string()) })
                    })
                    .collect()
            }
            "LOGIN" => {
                let mut v = vec![Row { lvl: '·', text: "Open the password repair console".into(), detail: "test your password (never counts towards a lockout) · reset a lockout · fix layout / PAM / keyring".into(), act: Act::Term("shadow-passfix".into()) }];
                let (_, o) = sh(&["shadow-passfix", "--check"]);
                for l in o.lines() {
                    let (k, t) = l.split_once(' ').unwrap_or(("ok", l));
                    v.push(Row { lvl: match k { "bad" => '✗', "warn" => '!', _ => '✓' }, text: t.to_string(), detail: String::new(), act: if k == "ok" { Act::None } else { Act::Term("shadow-passfix".into()) } });
                }
                v.push(Row { lvl: '·', text: "Can't log in at all? On a text console (Ctrl+Alt+F2) run:  shadow-passfix".into(), detail: String::new(), act: Act::None });
                v
            }
            "DESKTOP" => {
                let solid = conf_bool("look", "solid_background", true);
                let hub = conf_bool("hub", "enabled", true);
                vec![
                    Row {
                        lvl: if solid { '·' } else { '✓' },
                        text: format!("Solid background colour: {}", if solid { "ON" } else { "OFF" }),
                        detail: if solid { "Enter turns it off: no flat obsidian behind the desktop or the overview — only the hub's field or your wallpaper".into() } else { "Enter turns it back on (obsidian behind the overview and wherever nothing else is drawn)".into() },
                        act: Act::Toggle("look", "solid_background"),
                    },
                    Row {
                        lvl: '·',
                        text: format!("Desktop: {}", if hub { "the hub" } else { "the side bar + wallpaper" }),
                        detail: format!("Enter switches to {} now and for later logins", if hub { "the side bar + wallpaper" } else { "the hub" }),
                        act: Act::Work(vec!["shadow-hub".into(), if hub { "use-bar".into() } else { "switch".into() }]),
                    },
                    Row { lvl: '·', text: "Check the hub".into(), detail: "shadow-hub doctor: niri, config, processes, background, frame time, log".into(), act: Act::Term("shadow-hub doctor".into()) },
                    Row { lvl: '·', text: "Crash log".into(), detail: "what crashed or is holding changes back, with the details and a report to save".into(), act: Act::Exec("shadow-prefs crash".into()) },
                    Row { lvl: '·', text: "Default apps".into(), detail: "file manager, terminal and background".into(), act: Act::Exec("shadow-prefs".into()) },
                ]
            }
            _ => vec![
                Row { lvl: '·', text: "Update the system and Flatpaks, then triage".into(), detail: "a console: the package manager asks for your password; afterwards it lists what the updates changed and offers the repairs".into(), act: Act::Term("shadow fix update".into()) },
                Row { lvl: '·', text: "Triage now (after you updated another way)".into(), detail: "app versions that changed, launchers that went stale, caches to clear".into(), act: Act::Term("shadow fix triage".into()) },
                Row { lvl: '·', text: "Update Shadow itself".into(), detail: "the Update app: only Shadow's changed files, with undo".into(), act: Act::Term("exec shadow-update".into()) },
            ],
        };
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
    }

    fn activate(&mut self, ctx: &mut Ctx) {
        let Some(r) = self.rows.get(self.sel).cloned() else { return };
        match r.act {
            Act::Fix(id, true) => {
                term(&format!("shadow fix apply {id}"));
                self.msg = ("opened a terminal: sudo asks for your password there; press r to rescan afterwards".into(), false);
            }
            Act::Fix(id, false) => self.work(vec!["shadow".into(), "fix".into(), "apply".into(), id]),
            Act::Task(id) => self.work(vec!["shadow".into(), "fix".into(), "run".into(), id]),
            Act::Term(cmd) if cmd.starts_with("exec ") => shadow_apps::spawn(&cmd[5..]),
            Act::Term(cmd) => term(&cmd),
            Act::Exec(cmd) => shadow_apps::spawn(&cmd),
            Act::Toggle(section, key) => {
                let on = conf_bool(section, key, true);
                let p = shadow_apps::config_path();
                let t = shadow_apps::cfg::set_in(&std::fs::read_to_string(&p).unwrap_or_default(), section, key, if on { "false" } else { "true" });
                if let Err(e) = shadow_apps::atomic_write(&p, &t) {
                    self.msg = (format!("cannot write {}: {e}", p.display()), true);
                } else {
                    self.work(vec!["shadow".into(), "setup".into(), "niri".into()]);
                    self.load();
                    self.msg = (format!("{key} = {} — niri applies it now", !on), false);
                }
            }
            Act::Work(cmd) => {
                self.work(cmd);
            }
            Act::None => return,
        }
        ctx.redraw();
    }

    fn work(&mut self, cmd: Vec<String>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.msg = ("working…".into(), false);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let a: Vec<&str> = cmd.iter().map(String::as_str).collect();
            let (c, o) = sh(&a);
            let line = o.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
            let _ = tx.send(format!("{} {line}", if c == 0 { "ok" } else { "fail" }));
        });
    }
}

impl App for Fixc {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-fix".into(), title: "Fix center".into(), size: (960, 640) }
    }

    fn message(&mut self, m: String, ctx: &mut Ctx) {
        self.busy = false;
        let (k, t) = m.split_once(' ').unwrap_or((&m, ""));
        self.msg = (t.trim_start_matches(['✓', '✗', ' ']).to_string(), k != "ok");
        self.load();
        ctx.redraw();
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        let n = self.rows.len();
        match k.sym {
            Keysym::Escape | Keysym::q => ctx.quit(),
            Keysym::Tab | Keysym::ISO_Left_Tab => {
                self.page = if k.shift || k.sym == Keysym::ISO_Left_Tab { (self.page + PAGES.len() - 1) % PAGES.len() } else { (self.page + 1) % PAGES.len() };
                self.sel = 0;
                self.top = 0;
                self.msg.0.clear();
                self.load();
            }
            Keysym::Up => self.sel = self.sel.saturating_sub(1),
            Keysym::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            Keysym::r => {
                self.load();
                self.msg = ("rescanned".into(), false);
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => self.activate(ctx),
            _ => return,
        }
        ctx.redraw();
    }

    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        if let Some(&(p, _)) = self.tabs.iter().find(|(_, r)| r.hit(x, y)) {
            self.page = p;
            self.sel = 0;
            self.top = 0;
            self.msg.0.clear();
            self.load();
            ctx.redraw();
        } else if let Some(&(i, _)) = self.hits.iter().find(|(_, r)| r.hit(x, y)) {
            if self.sel == i {
                self.activate(ctx);
            } else {
                self.sel = i;
                ctx.redraw();
            }
        }
    }

    fn scroll(&mut self, lines: i32, ctx: &mut Ctx) {
        let n = self.rows.len() as i32;
        self.sel = (self.sel as i32 + lines).clamp(0, (n - 1).max(0)) as usize;
        ctx.redraw();
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let issues = self.rows.iter().filter(|r| r.lvl == '!' || r.lvl == '✗').count();
        let status = if self.busy { "working…".to_string() } else if PAGES[self.page] == "ISSUES" { format!("{issues} to fix") } else { String::new() };
        let (top, pad) = chrome(f, "FIX CENTER", &status);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt;
        let spt = pt * 0.84;
        let lh = f.line_h(pt);
        let slh = f.line_h(spt);
        // tabs
        self.tabs.clear();
        let mut x = pad;
        for (i, p) in PAGES.iter().enumerate() {
            let r = shadow_ui::widgets::button(f, x, top, p, i == self.page, true);
            self.tabs.push((i, r));
            x += r.w + f.px(8.0);
        }
        let list_top = top + lh + f.px(26.0);
        let row_h = lh + slh + f.px(12.0);
        let foot = f.line_h(pt * 0.88) + pad + f.px(14.0) + lh + f.px(6.0);
        self.visible = ((h - list_top - foot) / row_h).max(1) as usize;
        if self.sel < self.top {
            self.top = self.sel;
        }
        if self.sel >= self.top + self.visible {
            self.top = self.sel + 1 - self.visible;
        }
        self.hits.clear();
        for (i, r) in self.rows.iter().enumerate().skip(self.top).take(self.visible) {
            let y = list_top + (i - self.top) as i32 * row_h;
            let rect = Rect { x: pad, y, w: w - 2 * pad, h: row_h - f.px(4.0) };
            if i == self.sel {
                selection(f, rect.x, rect.y, rect.w, rect.h);
            }
            let col = match r.lvl {
                '!' => th.amber,
                '✗' => th.crimson,
                '✓' => th.green,
                _ => th.cyan,
            };
            let mx = pad + f.px(16.0);
            f.text(mx, y + f.px(4.0), &r.lvl.to_string(), pt, col, true);
            let tx = mx + f.px(22.0);
            let t = f.fit(&r.text, pt, i == self.sel, w - pad - tx - f.px(10.0));
            f.text(tx, y + f.px(4.0), &t, pt, if r.lvl == '✓' { th.dim } else { th.text }, i == self.sel);
            if !r.detail.is_empty() {
                let d = f.fit(&r.detail, spt, false, w - pad - tx - f.px(10.0));
                f.text(tx, y + f.px(4.0) + lh, &d, spt, th.dim, false);
            }
            self.hits.push((i, rect));
        }
        if !self.msg.0.is_empty() {
            let m = f.fit(&self.msg.0, pt * 0.9, false, w - 2 * pad);
            f.text(pad + f.px(8.0), h - foot + f.px(4.0), &m, pt * 0.9, if self.msg.1 { th.crimson } else { th.cyan }, false);
        }
        footer(f, "Tab page · ↑↓ select · Enter / click apply · r rescan · Esc close · text console: shadow fix · shadow-passfix");
    }
}

fn main() {
    let (tx, rx) = channel::channel::<String>();
    let page = std::env::args().nth(1).and_then(|a| PAGES.iter().position(|p| p.eq_ignore_ascii_case(&a))).unwrap_or(0);
    let mut s = Fixc { page, rows: vec![], sel: 0, top: 0, visible: 8, busy: false, msg: (String::new(), false), tx, hits: vec![], tabs: vec![] };
    s.load();
    if let Err(e) = run(Box::new(s), Some(rx)) {
        eprintln!("shadow-fix: {e}");
        std::process::exit(1);
    }
}
