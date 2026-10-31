//! shadow-update — the updater, its own window.
//!
//! Nothing happens until you ask: "Check for updates" fetches the configured repository and lists
//! the new commits, the changed source files and the components they affect. "Update" rebuilds
//! incrementally, compares the result byte for byte with what is installed and replaces only the
//! files that differ (with a backup); "Undo last update" restores that backup. Your config files
//! in /etc are never overwritten — a changed default lands next to yours as FILE.shadow-new.
//!
//! While an update writes files no new desktop session can start; if one is cut off, it's offered
//! here as "Recover interrupted update" (and recovered on its own at the next boot).
//!
//! A policy-compliant update source is required (see `shadow update setup`): until there is one,
//! only "Set up update source" is available; its rules are shown under the source line.
//!
//!   s  source     c  check      u  update      z  undo / recover      Esc  close      (or click)
use shadow_ui::calloop::channel::{self, Sender};
use shadow_ui::widgets::{button, chrome, footer, Input, Rect};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Job {
    Check,
    Apply,
    Undo,
}

#[derive(Clone, Copy, PartialEq)]
enum Btn {
    Setup,
    Check,
    Update,
    Undo,
    Close,
}
const BTNS: [Btn; 5] = [Btn::Setup, Btn::Check, Btn::Update, Btn::Undo, Btn::Close];

struct Upd {
    tx: Sender<String>,
    installed: String,
    source: String,
    busy: Option<Job>,
    status: String,
    message: String,
    commits: Vec<String>,
    changed: Vec<String>,
    components: Vec<String>,
    reinstall: Option<String>,
    log: Vec<String>,
    result: Option<(bool, String)>,
    /// password prompt for the administrator step: which job is waiting for it
    pw_for: Option<Job>,
    pw: Input,
    sel: usize,
    rects: Vec<(Btn, Rect)>,
    scroll: usize,
    /// an update is running elsewhere / was interrupted (from `shadow update in-progress`)
    pending: Option<String>,
    /// the update-source policy (offline rules): (level, name, detail)
    policy: Vec<(String, String, String)>,
}

fn shadow() -> String {
    if std::path::Path::new("/opt/shadow-tui/bin/shadow").exists() { "/opt/shadow-tui/bin/shadow".into() } else { "shadow".into() }
}

/// Run on a worker thread, sending each output line as `<tag>:<line>` and finally `end:<code>`.
fn worker(tx: Sender<String>, cmd: Vec<String>, stdin: Option<String>) {
    std::thread::spawn(move || {
        let child = Command::new(&cmd[0]).args(&cmd[1..]).stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
        let mut c = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(format!("log {}: {e}", cmd[0]));
                let _ = tx.send("end 127".into());
                return;
            }
        };
        if let (Some(mut s), Some(mut i)) = (stdin, c.stdin.take()) {
            let _ = i.write_all(s.as_bytes());
            wipe(&mut s);
        }
        let err = c.stderr.take();
        let tx2 = tx.clone();
        let t = std::thread::spawn(move || {
            if let Some(e) = err {
                for l in BufReader::new(e).lines().map_while(Result::ok) {
                    let _ = tx2.send(format!("log {l}"));
                }
            }
        });
        if let Some(o) = c.stdout.take() {
            for l in BufReader::new(o).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        }
        let _ = t.join();
        let code = c.wait().ok().and_then(|s| s.code()).unwrap_or(1);
        let _ = tx.send(format!("end {code}"));
    });
}

fn wipe(s: &mut String) {
    // SAFETY: zero bytes keep the String valid UTF-8
    unsafe { s.as_bytes_mut().iter_mut().for_each(|b| std::ptr::write_volatile(b, 0)) };
    s.clear();
}

impl Upd {
    fn policy_ok(&self) -> bool {
        !self.policy.is_empty() && !self.policy.iter().any(|(l, _, _)| l == "fail")
    }

    fn load_policy(&mut self) {
        let (_, o) = shadow_apps::cmd_out(&[&shadow(), "update", "policy", "--offline", "--porcelain"]);
        self.policy = o
            .lines()
            .filter_map(|l| {
                let (lv, r) = l.split_once(' ')?;
                let (n, d) = r.split_once(' ').unwrap_or((r, ""));
                ["pass", "warn", "fail"].contains(&lv).then(|| (lv.to_string(), n.to_string(), d.to_string()))
            })
            .collect();
    }

    fn interrupted(&self) -> bool {
        self.pending.as_deref().is_some_and(|p| p.contains("interrupted"))
    }

    fn load_status(&mut self) {
        self.load_policy();
        let (rc, o) = shadow_apps::cmd_out(&[&shadow(), "update", "in-progress"]);
        self.pending = (rc == 0).then(|| o.trim().to_string());
        let (_, o) = shadow_apps::cmd_out(&[&shadow(), "update", "status"]);
        for l in o.lines() {
            if let Some(v) = l.strip_prefix("installed :") {
                self.installed = v.trim().to_string();
            } else if let Some(v) = l.strip_prefix("repository:") {
                self.source = v.trim().replace("  (checked only when you ask)", "");
            }
        }
    }

    fn enabled(&self, b: Btn) -> bool {
        match b {
            Btn::Setup => self.busy.is_none(),
            Btn::Check => self.busy.is_none() && self.policy_ok(),
            Btn::Update => self.busy.is_none() && self.policy_ok() && self.pending.is_none() && self.status == "available" && self.reinstall.is_none(),
            Btn::Undo => self.busy.is_none() && (self.pending.is_none() || self.interrupted()),
            Btn::Close => true,
        }
    }

    fn press(&mut self, b: Btn, ctx: &mut Ctx) {
        if !self.enabled(b) {
            return;
        }
        match b {
            // the wizard is interactive (it shows the deploy key to add): it runs in a terminal
            Btn::Setup => shadow_apps::spawn("shadow-term --hold -e shadow update setup"),
            Btn::Check => self.start(Job::Check, None),
            Btn::Update | Btn::Undo => {
                let job = if b == Btn::Update { Job::Apply } else { Job::Undo };
                // administrator rights: no prompt when sudo doesn't need a password right now
                if shadow_apps::cmd_out(&["sudo", "-n", "true"]).0 == 0 {
                    self.start(job, None);
                } else {
                    self.pw_for = Some(job);
                    self.pw.clear();
                }
            }
            Btn::Close => ctx.quit(),
        }
        ctx.redraw();
    }

    fn start(&mut self, job: Job, password: Option<String>) {
        self.busy = Some(job);
        self.result = None;
        self.scroll = 0;
        let sh = shadow();
        let cmd: Vec<String> = match job {
            Job::Check => {
                self.commits.clear();
                self.changed.clear();
                self.components.clear();
                self.reinstall = None;
                self.status = "checking".into();
                self.message = "fetching the update source…".into();
                vec![sh, "update".into(), "check-files".into(), "--porcelain".into()]
            }
            Job::Apply => {
                self.log.clear();
                let mut v = vec![sh, "update".into(), "apply".into(), "--porcelain".into()];
                if password.is_some() {
                    v.push("--password-stdin".into());
                }
                v
            }
            Job::Undo => {
                self.log.clear();
                // undo on an interrupted update = recover it (finish or reverse)
                let verb = if self.interrupted() { "recover" } else { "undo" };
                if password.is_some() {
                    vec!["sudo".into(), "-S".into(), "-p".into(), String::new(), sh, "update".into(), verb.into()]
                } else {
                    vec!["sudo".into(), "-n".into(), sh, "update".into(), verb.into()]
                }
            }
        };
        worker(self.tx.clone(), cmd, password.map(|p| format!("{p}\n")));
    }

    fn line(&mut self, l: &str) {
        let (k, v) = l.split_once(' ').unwrap_or((l, ""));
        let job = self.busy;
        match (job, k) {
            (_, "end") => {
                let ok = v.trim() == "0";
                match job {
                    Some(Job::Check) if !ok && self.status == "checking" => {
                        self.status = "error".into();
                    }
                    Some(Job::Apply) if self.result.is_none() => self.result = Some((ok, if ok { "updated".into() } else { "the update did not complete".into() })),
                    Some(Job::Undo) => {
                        let last = self.log.last().cloned().unwrap_or_default();
                        self.result = Some((ok, if ok { last } else { format!("undo failed: {last}") }));
                    }
                    _ => {}
                }
                self.busy = None;
                if job != Some(Job::Check) {
                    self.load_status();
                    if self.result.as_ref().is_some_and(|r| r.0) {
                        self.status = "applied".into();
                    }
                }
            }
            (Some(Job::Check), "installed") => {}
            (Some(Job::Check), "latest") => {}
            (Some(Job::Check), "status") => self.status = v.into(),
            (Some(Job::Check), "message") => self.message = v.into(),
            (Some(Job::Check), "commit") => self.commits.push(v.into()),
            (Some(Job::Check), "changed") => self.changed.push(v.into()),
            (Some(Job::Check), "component") => self.components.push(v.into()),
            (Some(Job::Check), "reinstall") => self.reinstall = Some(v.into()),
            (Some(Job::Apply), "result") => {
                let (st, msg) = v.split_once(' ').unwrap_or((v, ""));
                self.result = Some((st == "ok", msg.to_string()));
            }
            (Some(Job::Apply), "step") => self.log.push(format!("▸ {v}")),
            (Some(Job::Apply), "replace" | "new" | "remove" | "config") => self.log.push(format!("  {k:<8} {v}")),
            (_, "log") => self.log.push(format!("  {v}")),
            _ => self.log.push(format!("  {l}")),
        }
        if self.log.len() > 2000 {
            self.log.drain(..500);
        }
    }
}

impl App for Upd {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-update".into(), title: "Update".into(), size: (760, 560) }
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        if let Some(job) = self.pw_for {
            match k.sym {
                Keysym::Escape => {
                    self.pw_for = None;
                    let mut t = std::mem::take(&mut self.pw.text);
                    wipe(&mut t);
                    self.pw.clear();
                }
                Keysym::Return | Keysym::KP_Enter => {
                    self.pw_for = None;
                    let pw = std::mem::take(&mut self.pw.text);
                    self.pw.clear();
                    self.start(job, Some(pw));
                }
                _ => {
                    self.pw.key(k);
                }
            }
            ctx.redraw();
            return;
        }
        match k.sym {
            Keysym::Escape | Keysym::q => ctx.quit(),
            Keysym::c => self.press(Btn::Check, ctx),
            Keysym::s => self.press(Btn::Setup, ctx),
            Keysym::u => self.press(Btn::Update, ctx),
            Keysym::z => self.press(Btn::Undo, ctx),
            Keysym::Left | Keysym::Right | Keysym::Tab => {
                let n = BTNS.len();
                self.sel = if k.sym == Keysym::Left { (self.sel + n - 1) % n } else { (self.sel + 1) % n };
                ctx.redraw();
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => self.press(BTNS[self.sel], ctx),
            Keysym::Up => {
                self.scroll += 1;
                ctx.redraw();
            }
            Keysym::Down => {
                self.scroll = self.scroll.saturating_sub(1);
                ctx.redraw();
            }
            _ => {}
        }
    }

    fn tick_ms(&self) -> u64 {
        3000
    }

    /// While the source isn't set up yet (the wizard runs in its own terminal), re-read the policy.
    fn tick(&mut self, ctx: &mut Ctx) {
        if self.busy.is_some() || self.policy_ok() {
            return;
        }
        let before = self.policy.clone();
        self.load_status();
        if self.policy != before {
            ctx.redraw();
        }
    }

    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        if let Some(&(b, _)) = self.rects.iter().find(|(_, r)| r.hit(x, y)) {
            self.sel = BTNS.iter().position(|&x| x == b).unwrap_or(0);
            self.press(b, ctx);
        }
    }

    fn scroll(&mut self, lines: i32, ctx: &mut Ctx) {
        self.scroll = if lines < 0 { self.scroll + (-lines) as usize } else { self.scroll.saturating_sub(lines as usize) };
        ctx.redraw();
    }

    fn message(&mut self, m: String, ctx: &mut Ctx) {
        self.line(&m);
        ctx.redraw();
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let busy = match self.busy {
            Some(Job::Check) => "checking…",
            Some(Job::Apply) => "updating…",
            Some(Job::Undo) => "undoing…",
            None => "",
        };
        let (top, pad) = chrome(f, "UPDATE", busy);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt;
        let small = pt * 0.9;
        let lh = f.line_h(pt);
        let slh = f.line_h(small);
        let x0 = pad + f.px(8.0);
        let mut y = top;
        let kv = |f: &mut Frame, y: i32, k: &str, v: &str| {
            let kw = f.text(x0, y, k, pt, th.dim, false);
            let v = f.fit(v, pt, false, w - 2 * pad - kw - f.px(20.0));
            f.text(x0 + kw, y, &v, pt, th.text, false);
        };
        kv(f, y, "installed  ", if self.installed.is_empty() { "?" } else { &self.installed });
        y += lh + f.px(2.0);
        kv(f, y, "source     ", if self.source.is_empty() { "(required — press “Set up update source”)" } else { &self.source });
        y += lh + f.px(4.0);
        // the policy every fetch must satisfy (offline rules; access + privacy are checked on Check)
        let mut px = x0;
        for (lv, name, detail) in &self.policy {
            let (mark, col) = match lv.as_str() {
                "pass" => ("✓", th.green),
                "warn" => ("!", th.amber),
                _ => ("✗", th.crimson),
            };
            let item = format!("{mark} {name}  ");
            let iw = f.text_w(&item, small, false);
            if px + iw > w - pad {
                px = x0;
                y += slh;
            }
            f.text(px, y, &item, small, col, lv == "fail");
            px += iw;
            let _ = detail;
        }
        y += slh + f.px(2.0);
        if let Some((_, name, detail)) = self.policy.iter().find(|(l, _, _)| l == "fail") {
            let m = f.fit(&format!("{name}: {detail}"), small, false, w - 2 * pad - f.px(16.0));
            f.text(x0, y, &m, small, th.crimson, false);
            y += slh;
        }
        y += f.px(8.0);

        // buttons
        self.rects.clear();
        let mut bx = x0;
        for (i, b) in BTNS.iter().enumerate() {
            let label = match b {
                Btn::Setup => if self.policy_ok() { "Update source…" } else { "Set up update source" },
                Btn::Check => "Check for updates",
                Btn::Update => "Update",
                Btn::Undo => if self.interrupted() { "Recover interrupted update" } else { "Undo last update" },
                Btn::Close => "Close",
            };
            let r = button(f, bx, y, label, i == self.sel && self.pw_for.is_none(), self.enabled(*b));
            self.rects.push((*b, r));
            bx += r.w + f.px(10.0);
        }
        y += self.rects.first().map_or(lh, |r| r.1.h) + f.px(14.0);

        // an update in flight elsewhere, or one cut off: the desktop can't be started meanwhile
        if let Some(p) = &self.pending {
            let msg = if self.interrupted() {
                format!("! Interrupted update ({p}). New logins are locked until it is recovered: press Recover (or reboot)")
            } else {
                format!("! An update is running ({p}). New logins wait until it finishes.")
            };
            let m = f.fit(&msg, small, true, w - 2 * pad - f.px(16.0));
            f.text(x0, y, &m, small, th.crimson, true);
            y += slh + f.px(8.0);
        }

        // password prompt
        if let Some(job) = self.pw_for {
            f.cv.fill(pad, y, w - 2 * pad, lh * 2 + f.px(22.0), th.panel, 1.0);
            f.cv.border(pad, y, w - 2 * pad, lh * 2 + f.px(22.0), f.hair(), th.amber, 0.9);
            let what = if job == Job::Apply { "install the changed files" } else { "restore the backup" };
            f.text(x0, y + f.px(6.0), &format!("Administrator password to {what}:"), pt, th.amber, true);
            let px = x0 + f.text(x0, y + lh + f.px(12.0), "▸ ", pt, th.amber, true);
            self.pw.draw(f, px, y + lh + f.px(12.0), pt, true);
            y += lh * 2 + f.px(34.0);
        }

        // status / result
        let (col, headline) = match (&self.result, self.status.as_str()) {
            (Some((true, m)), _) => (th.green, format!("✓ {m}")),
            (Some((false, m)), _) => (th.crimson, format!("✗ {m}")),
            (None, "available") => (th.amber, format!("Update available — {}", self.message)),
            (None, "up-to-date") => (th.green, format!("✓ {}", self.message)),
            (None, "error" | "not-configured") => (th.crimson, format!("✗ {}", self.message)),
            (None, "checking") => (th.cyan, self.message.clone()),
            _ if !self.policy_ok() => (th.crimson, "An update source that meets the policy is required before anything can be checked.".into()),
            _ => (th.dim, "Nothing is checked automatically. Press “Check for updates” when you want to.".into()),
        };
        let hl = f.fit(&headline, pt, true, w - 2 * pad - f.px(16.0));
        f.text(x0, y, &hl, pt, col, true);
        y += lh + f.px(8.0);
        if let Some(r) = &self.reinstall {
            let s = f.fit(&format!("! {r}: run ./install.sh for this one"), small, false, w - 2 * pad - f.px(16.0));
            f.text(x0, y, &s, small, th.crimson, false);
            y += slh + f.px(6.0);
        }

        // body: commits + affected components after a check, the live log during/after an update
        let foot = f.line_h(small) + pad + f.px(10.0);
        let bottom = h - foot;
        let mut body: Vec<(String, shadow_ui::Rgb)> = vec![];
        if !self.log.is_empty() {
            for l in &self.log {
                let c = if l.starts_with('▸') { th.amber } else if l.trim_start().starts_with("replace") || l.trim_start().starts_with("new") { th.cyan } else { th.dim };
                body.push((l.clone(), c));
            }
        } else if !self.commits.is_empty() || !self.components.is_empty() {
            body.push((format!("Affected ({} source file(s) changed):", self.changed.len()), th.amber));
            for c in &self.components {
                body.push((format!("  • {c}"), th.text));
            }
            body.push((String::new(), th.dim));
            body.push((format!("Commits ({}):", self.commits.len()), th.amber));
            for c in &self.commits {
                body.push((format!("  {c}"), th.text));
            }
            body.push((String::new(), th.dim));
            body.push(("Only installed files whose bytes differ will be replaced; unchanged files are left alone.".into(), th.dim));
        }
        let rows = ((bottom - y) / slh).max(0) as usize;
        let max_scroll = body.len().saturating_sub(rows);
        self.scroll = self.scroll.min(max_scroll);
        // the log follows its tail; the scroll offset counts back from the end
        let start = if !self.log.is_empty() { max_scroll - self.scroll } else { self.scroll.min(max_scroll) };
        for (l, c) in body.iter().skip(start).take(rows) {
            let s = f.fit(l, small, false, w - 2 * pad - f.px(16.0));
            f.text(x0, y, &s, small, *c, false);
            y += slh;
        }
        footer(f, "s source · c check · u update · z undo · ←→ Enter or click buttons · wheel scroll · Esc close");
    }
}

fn main() {
    let (tx, rx) = channel::channel::<String>();
    let mut u = Upd {
        tx,
        installed: String::new(),
        source: String::new(),
        busy: None,
        status: String::new(),
        message: String::new(),
        commits: vec![],
        changed: vec![],
        components: vec![],
        reinstall: None,
        log: vec![],
        result: None,
        pw_for: None,
        pw: Input::secret(),
        sel: 0,
        rects: vec![],
        scroll: 0,
        pending: None,
        policy: vec![],
    };
    u.load_status();
    if let Err(e) = run(Box::new(u), Some(rx)) {
        eprintln!("shadow-update: {e}");
        std::process::exit(1);
    }
}
