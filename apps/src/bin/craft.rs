//! shadow-craft: the Craft App Suite (ArtCraft) for Shadow. Install, update, remove and launch
//! PhotoCraft, VectorCraft, FilmCraft and the rest, per user and with no password.
//!
//! Usage:
//!
//!   shadow-craft                the window
//!   shadow-craft list           installed and newest versions, as text
//!   shadow-craft install ID     install or update one app (e.g. photocraft)
//!   shadow-craft remove ID
//!   shadow-craft update         update every installed app that has a newer release
//!
//! Every download is checked against the SHA-256 that GitHub publishes for it before it is
//! installed (see shadow_apps::craft). Newest versions are cached for an hour to stay under
//! GitHub's limit for anonymous requests.
//!
//! Keys: Enter install / update / launch · d remove (twice) · o releases page · u update all ·
//! r check again · Esc close
use shadow_apps::craft::{self, App as CraftApp, Installed, Release, SUITE};
use shadow_apps::{atomic_write, home, spawn};
use shadow_ui::calloop::channel::{self, Sender};
use shadow_ui::widgets::{chrome, footer, selection, Rect};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

#[derive(Clone)]
enum Latest {
    Checking,
    Version(String),
    Error(String),
}

struct Craft {
    installed: BTreeMap<String, Installed>,
    latest: HashMap<String, Latest>,
    releases: HashMap<String, Release>,
    busy: HashMap<String, String>,
    sel: usize,
    top: usize,
    visible: usize,
    armed: Option<usize>,
    msg: (String, bool),
    rows: Vec<(usize, Rect)>,
    tx: Sender<String>,
}

fn cache_path() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".cache")).join("shadow-craft/latest")
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// cached newest versions younger than an hour: id → (version, release json)
fn cached() -> HashMap<String, String> {
    let mut m = HashMap::new();
    for l in std::fs::read_to_string(cache_path()).unwrap_or_default().lines() {
        let mut w = l.splitn(3, ' ');
        if let (Some(id), Some(t), Some(json)) = (w.next(), w.next(), w.next()) {
            if t.parse::<u64>().is_ok_and(|t| now().saturating_sub(t) < 3600) {
                m.insert(id.to_string(), json.to_string());
            }
        }
    }
    m
}

fn remember(id: &str, json: &str) {
    let mut lines: Vec<String> = std::fs::read_to_string(cache_path()).unwrap_or_default().lines().filter(|l| !l.starts_with(&format!("{id} "))).map(String::from).collect();
    lines.push(format!("{id} {} {}", now(), json.replace('\n', " ")));
    let _ = atomic_write(&cache_path(), &(lines.join("\n") + "\n"));
}

/// fetch (or take from the cache) the newest release of every app, one message each
fn check_all(tx: Sender<String>, force: bool) {
    std::thread::spawn(move || {
        let cache = if force { HashMap::new() } else { cached() };
        for app in SUITE.iter() {
            let json = match cache.get(app.id) {
                Some(j) => Ok(j.clone()),
                None => {
                    let url = format!("https://api.github.com/repos/{}/releases/latest", app.repo);
                    let (c, o) = shadow_apps::cmd_out(&["curl", "-fsSL", "-m", "30", "-H", "Accept: application/vnd.github+json", &url]);
                    if c == 0 {
                        remember(app.id, &o);
                        Ok(o)
                    } else if c == 127 {
                        Err("curl is not installed".to_string())
                    } else {
                        Err(format!("could not reach GitHub{}", if o.contains("403") || o.contains("429") { " (rate limit: try again in an hour)" } else { "" }))
                    }
                }
            };
            let msg = match json {
                Ok(j) => format!("rel {} {}", app.id, j.replace('\n', " ")),
                Err(e) => format!("err {} {e}", app.id),
            };
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
}

impl Craft {
    fn app(&self) -> &'static CraftApp {
        &SUITE[self.sel.min(SUITE.len() - 1)]
    }

    fn status(&self, app: &CraftApp) -> (String, char) {
        if let Some(b) = self.busy.get(app.id) {
            return (b.clone(), '·');
        }
        let inst = self.installed.get(app.id);
        match (inst, self.latest.get(app.id)) {
            (Some(i), Some(Latest::Version(v))) if craft::newer(v, &i.version) => (format!("{} → {v} available", i.version), '!'),
            (Some(i), _) => (format!("{} installed", i.version), '✓'),
            (None, Some(Latest::Version(v))) => (format!("{v} available"), '·'),
            (None, Some(Latest::Checking)) | (None, None) => ("checking…".into(), '·'),
            (None, Some(Latest::Error(e))) => (e.clone(), '✗'),
        }
    }

    fn act(&mut self, update_only: bool) {
        let app = self.app();
        if self.busy.contains_key(app.id) {
            return;
        }
        let inst = self.installed.get(app.id).cloned();
        let rel = self.releases.get(app.id).cloned();
        match (inst, rel) {
            (Some(i), Some(r)) if craft::newer(&r.version, &i.version) => self.start_install(app, r),
            (Some(i), _) if !update_only => {
                spawn(&craft::launch_cmd(&i));
                self.msg = (format!("started {}", app.name), false);
            }
            (None, Some(r)) if !update_only => self.start_install(app, r),
            (None, None) if !update_only => self.msg = (format!("{}: newest version not known yet (r checks again)", app.name), true),
            _ => {}
        }
    }

    fn start_install(&mut self, app: &'static CraftApp, rel: Release) {
        self.busy.insert(app.id.to_string(), format!("downloading {}…", rel.version));
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let msg = match craft::install(app, &rel) {
                Ok(i) => format!("done {} ok {} {}", app.id, i.version, i.kind.name()),
                Err(e) => format!("done {} fail {e}", app.id),
            };
            let _ = tx.send(msg);
        });
    }
}

impl App for Craft {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-craft".into(), title: "Craft apps".into(), size: (980, 700) }
    }

    fn message(&mut self, m: String, ctx: &mut Ctx) {
        let mut w = m.splitn(3, ' ');
        let (kind, id, rest) = (w.next().unwrap_or(""), w.next().unwrap_or("").to_string(), w.next().unwrap_or("").to_string());
        match kind {
            "rel" => match craft::parse_release(&rest) {
                Ok(r) => {
                    self.latest.insert(id.clone(), Latest::Version(r.version.clone()));
                    self.releases.insert(id, r);
                }
                Err(e) => {
                    self.latest.insert(id, Latest::Error(e));
                }
            },
            "err" => {
                self.latest.insert(id, Latest::Error(rest));
            }
            "done" => {
                self.busy.remove(&id);
                self.installed = craft::installed();
                let name = craft::by_id(&id).map(|a| a.name).unwrap_or("");
                self.msg = match rest.split_once(' ') {
                    Some(("ok", v)) => (format!("{name} {} installed — it is in the launcher and the hub's APPS", v.split(' ').next().unwrap_or("")), false),
                    Some((_, e)) => (e.to_string(), true),
                    None => (rest.clone(), true),
                };
            }
            _ => {}
        }
        ctx.redraw();
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        let n = SUITE.len();
        if k.sym != Keysym::d {
            self.armed = None;
        }
        match k.sym {
            Keysym::Escape | Keysym::q => ctx.quit(),
            Keysym::Up | Keysym::k => self.sel = self.sel.saturating_sub(1),
            Keysym::Down | Keysym::j => self.sel = (self.sel + 1).min(n - 1),
            Keysym::Return | Keysym::KP_Enter => self.act(false),
            Keysym::d => {
                let app = self.app();
                if !self.installed.contains_key(app.id) {
                    return;
                }
                if self.armed == Some(self.sel) {
                    self.armed = None;
                    self.msg = match craft::remove(app) {
                        Ok(()) => (format!("{} removed", app.name), false),
                        Err(e) => (e, true),
                    };
                    self.installed = craft::installed();
                } else {
                    self.armed = Some(self.sel);
                    self.msg = (format!("press d again to remove {}", app.name), false);
                }
            }
            Keysym::o => spawn(&format!("xdg-open https://github.com/{}/releases", self.app().repo)),
            Keysym::u => {
                let keep = self.sel;
                for i in 0..n {
                    self.sel = i;
                    if self.installed.contains_key(SUITE[i].id) {
                        self.act(true);
                    }
                }
                self.sel = keep;
                if self.busy.is_empty() {
                    self.msg = ("every installed app is up to date".into(), false);
                }
            }
            Keysym::r => {
                for a in SUITE.iter() {
                    self.latest.insert(a.id.to_string(), Latest::Checking);
                }
                check_all(self.tx.clone(), true);
                self.msg = ("checking GitHub for the newest versions…".into(), false);
            }
            _ => return,
        }
        ctx.redraw();
    }

    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        if let Some(&(i, _)) = self.rows.iter().find(|(_, r)| r.hit(x, y)) {
            if self.sel == i {
                self.act(false);
            } else {
                self.sel = i;
            }
            ctx.redraw();
        }
    }

    fn scroll(&mut self, lines: i32, ctx: &mut Ctx) {
        self.sel = (self.sel as i32 + lines).clamp(0, SUITE.len() as i32 - 1) as usize;
        ctx.redraw();
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let n_inst = self.installed.len();
        let n_upd = SUITE.iter().filter(|a| matches!((self.installed.get(a.id), self.latest.get(a.id)), (Some(i), Some(Latest::Version(v))) if craft::newer(v, &i.version))).count();
        let status = format!("{n_inst} installed{}", if n_upd > 0 { format!(" · {n_upd} to update") } else { String::new() });
        let (top, pad) = chrome(f, "CRAFT APPS", &status);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt;
        let spt = pt * 0.84;
        let lh = f.line_h(pt);
        let slh = f.line_h(spt);
        let row_h = lh + slh + f.px(12.0);
        let foot = f.line_h(pt * 0.88) + pad + f.px(14.0) + lh + f.px(6.0);
        self.visible = ((h - top - foot) / row_h).max(1) as usize;
        if self.sel < self.top {
            self.top = self.sel;
        }
        if self.sel >= self.top + self.visible {
            self.top = self.sel + 1 - self.visible;
        }
        self.rows.clear();
        for (i, app) in SUITE.iter().enumerate().skip(self.top).take(self.visible) {
            let y = top + (i - self.top) as i32 * row_h;
            let r = Rect { x: pad, y, w: w - 2 * pad, h: row_h - f.px(4.0) };
            if i == self.sel {
                selection(f, r.x, r.y, r.w, r.h);
            }
            // the app's colour as a chamfered tile
            let ts = lh + slh - f.px(2.0);
            let tx = pad + f.px(18.0);
            let pts = shadow_ui::widgets::chamfer_pts(tx, y + f.px(4.0), ts, ts, ts / 4);
            let c = app.color;
            f.cv.poly(&pts, 1.0, &|_, fy| shadow_ui::canvas::lerp(c, (c.0 / 2, c.1 / 2, c.2 / 2), fy));
            let initial: String = app.name.chars().next().into_iter().collect();
            let iw = f.text_w(&initial, pt, true);
            f.text(tx + (ts - iw) / 2, y + f.px(4.0) + (ts - lh) / 2, &initial, pt, (255, 255, 255), true);
            let x = tx + ts + f.px(14.0);
            let (st, lvl) = self.status(app);
            let col = match lvl {
                '!' => th.amber,
                '✗' => th.crimson,
                '✓' => th.green,
                _ => th.cyan,
            };
            let sw = f.text_w(&st, spt, false).min(w / 3);
            let st = f.fit(&st, spt, false, w / 3);
            f.text(w - pad - f.px(14.0) - sw, y + f.px(6.0), &st, spt, col, false);
            f.text(x, y + f.px(4.0), app.name, pt, th.text, i == self.sel);
            let tl = f.fit(app.tagline, spt, false, w - pad - x - sw - f.px(30.0));
            f.text(x, y + f.px(4.0) + lh, &tl, spt, th.dim, false);
            self.rows.push((i, r));
        }
        if !self.msg.0.is_empty() {
            let m = f.fit(&self.msg.0, pt, false, w - 2 * pad);
            f.text(pad + f.px(8.0), h - foot + f.px(2.0), &m, pt, if self.msg.1 { th.crimson } else { th.green }, false);
        }
        footer(f, "Enter install / update / launch · d remove · o releases page · u update all · r check again · Esc close");
    }
}

fn cli(args: &[String]) -> i32 {
    let find = |id: &str| craft::by_id(id).ok_or_else(|| format!("unknown app {id:?} (shadow-craft list)"));
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["list"] => {
            let inst = craft::installed();
            for app in SUITE.iter() {
                let latest = craft::fetch_release(app).map(|r| r.version).unwrap_or_else(|e| format!("? ({e})"));
                println!("{:<15} {:<12} newest {latest}", app.id, inst.get(app.id).map(|i| i.version.clone()).unwrap_or_else(|| "—".into()));
            }
            0
        }
        ["install", id] => match find(id).and_then(|a| craft::fetch_release(a).and_then(|r| craft::install(a, &r)).map(|i| (a, i))) {
            Ok((a, i)) => {
                println!("{} {} installed ({})", a.name, i.version, i.exe.display());
                0
            }
            Err(e) => {
                eprintln!("shadow-craft: {e}");
                1
            }
        },
        ["remove", id] => match find(id).and_then(craft::remove) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("shadow-craft: {e}");
                1
            }
        },
        ["update"] => {
            let mut rc = 0;
            for (id, i) in craft::installed() {
                let Some(app) = craft::by_id(&id) else { continue };
                match craft::fetch_release(app) {
                    Ok(r) if craft::newer(&r.version, &i.version) => match craft::install(app, &r) {
                        Ok(n) => println!("{} {} → {}", app.name, i.version, n.version),
                        Err(e) => {
                            eprintln!("{}: {e}", app.name);
                            rc = 1;
                        }
                    },
                    Ok(_) => println!("{} {} is the newest", app.name, i.version),
                    Err(e) => {
                        eprintln!("{}: {e}", app.name);
                        rc = 1;
                    }
                }
            }
            rc
        }
        _ => {
            eprintln!("usage: shadow-craft [list | install ID | remove ID | update]");
            2
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        std::process::exit(cli(&args));
    }
    let (tx, rx) = channel::channel::<String>();
    let mut c = Craft { installed: craft::installed(), latest: HashMap::new(), releases: HashMap::new(), busy: HashMap::new(), sel: 0, top: 0, visible: 8, armed: None, msg: (String::new(), false), rows: vec![], tx: tx.clone() };
    for a in SUITE.iter() {
        c.latest.insert(a.id.to_string(), Latest::Checking);
    }
    check_all(tx, false);
    if let Err(e) = run(Box::new(c), Some(rx)) {
        eprintln!("shadow-craft: {e}");
        std::process::exit(1);
    }
}
