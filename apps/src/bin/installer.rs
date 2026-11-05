//! shadow-installer: the graphical front of ./install.sh (./install.sh --gui).
//!
//! Two ways in:
//!   FULL DESKTOP — Shadow TUI Restricted on its own compositor: the shell, every app, the power daemon, Wi-Fi
//!                  repair, updates; optionally the login screen and the security package.
//!   APPS ONLY    — pick the apps you want and keep your current desktop. They are installed for
//!                  your user only (~/.local), with no system services, no session and no login
//!                  screen changes; ./install.sh --remove-apps takes them out again.
//!
//! The window only decides; install.sh does the work in the terminal it was started from (that is
//! where sudo asks for your password). The choice is written to $SHADOW_INSTALL_CHOICE (or stdout):
//!   full [--greeter] [--sec]      apps shadow-term shadow-files …      cancel
//!
//! Keys: ↑↓ choose · Space tick · Enter continue / install · Tab next · Esc back / cancel.
//! Everything works with the mouse too.
use shadow_ui::widgets::{button, chrome, footer, panel, selection, Rect};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};

/// (program, name, what it is, caveat)
pub const APPS: &[(&str, &str, &str, &str)] = &[
    ("shadow-term", "Terminal", "Shadow's terminal: fast, sharp at any scale, the amber look", ""),
    ("shadow-files", "Files", "a keyboard file manager with previews, marks, copy / move", ""),
    ("shadow-launch", "Launcher", "search apps, files and actions from one box", "give it a key in your desktop's settings"),
    ("shadow-tasks", "Task manager", "CPU, memory, GPU, NPU, power and processes", ""),
    ("shadow-craft", "Craft apps", "install and update the ArtCraft suite (PhotoCraft, FilmCraft …)", "needs curl"),
    ("shadow-wallpaper", "3D wallpaper", "the vertex background that follows the cursor", "KDE, Sway or Hyprland — not GNOME"),
];

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Choose,
    Full,
    Apps,
}

struct Installer {
    page: Page,
    /// Choose: 0 full, 1 apps · Full: 0 greeter, 1 sec, 2 install · Apps: 0..n apps, n install
    sel: usize,
    picked: Vec<bool>,
    greeter: bool,
    sec: bool,
    os: String,
    hits: Vec<(usize, Rect)>,
    back: Option<Rect>,
    msg: String,
}

fn finish(choice: &str, ctx: &mut Ctx) {
    match std::env::var_os("SHADOW_INSTALL_CHOICE").filter(|v| !v.is_empty()) {
        Some(p) => {
            let _ = std::fs::write(p, format!("{choice}\n"));
        }
        None => println!("{choice}"),
    }
    ctx.quit();
}

impl Installer {
    fn rows(&self) -> usize {
        match self.page {
            Page::Choose => 2,
            Page::Full => 3,
            Page::Apps => APPS.len() + 1,
        }
    }

    fn activate(&mut self, ctx: &mut Ctx) {
        match self.page {
            Page::Choose => {
                self.page = if self.sel == 0 { Page::Full } else { Page::Apps };
                self.sel = if self.page == Page::Full { 2 } else { 0 };
            }
            Page::Full => match self.sel {
                0 => self.greeter = !self.greeter,
                1 => self.sec = !self.sec,
                _ => {
                    let mut c = "full".to_string();
                    if self.greeter {
                        c += " --greeter";
                    }
                    if self.sec {
                        c += " --sec";
                    }
                    finish(&c, ctx);
                }
            },
            Page::Apps => {
                if self.sel < APPS.len() {
                    self.picked[self.sel] = !self.picked[self.sel];
                    self.msg.clear();
                } else {
                    let list: Vec<&str> = APPS.iter().zip(&self.picked).filter(|(_, p)| **p).map(|(a, _)| a.0).collect();
                    if list.is_empty() {
                        self.msg = "tick at least one app (Space or click)".into();
                    } else {
                        finish(&format!("apps {}", list.join(" ")), ctx);
                    }
                }
            }
        }
    }

    fn go_back(&mut self, ctx: &mut Ctx) {
        if self.page == Page::Choose {
            finish("cancel", ctx);
        } else {
            self.sel = if self.page == Page::Full { 0 } else { 1 };
            self.page = Page::Choose;
            self.msg.clear();
        }
    }
}

impl App for Installer {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-installer".into(), title: "Install Shadow".into(), size: (900, 640) }
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        let n = self.rows();
        match k.sym {
            Keysym::Escape => self.go_back(ctx),
            Keysym::Up | Keysym::k => self.sel = self.sel.saturating_sub(1),
            Keysym::Down | Keysym::j | Keysym::Tab => self.sel = (self.sel + 1) % n,
            Keysym::Left if self.page == Page::Choose => self.sel = 0,
            Keysym::Right if self.page == Page::Choose => self.sel = 1,
            Keysym::space if self.page != Page::Choose => {
                // Space ticks; on the install button it installs
                self.activate(ctx);
            }
            Keysym::Return | Keysym::KP_Enter => {
                if self.page == Page::Apps && self.sel < APPS.len() {
                    // Enter on an app goes to the install button (Space ticks)
                    self.sel = APPS.len();
                } else {
                    self.activate(ctx);
                }
            }
            Keysym::a if self.page == Page::Apps => {
                let all = self.picked.iter().all(|p| *p);
                self.picked.iter_mut().for_each(|p| *p = !all);
            }
            _ => return,
        }
        ctx.redraw();
    }

    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        if self.back.is_some_and(|r| r.hit(x, y)) {
            self.go_back(ctx);
        } else if let Some(&(i, _)) = self.hits.iter().find(|(_, r)| r.hit(x, y)) {
            self.sel = i;
            self.activate(ctx);
        } else {
            return;
        }
        ctx.redraw();
    }

    fn scroll(&mut self, lines: i32, ctx: &mut Ctx) {
        self.sel = (self.sel as i32 + lines).clamp(0, self.rows() as i32 - 1) as usize;
        ctx.redraw();
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let (top, pad) = chrome(f, "INSTALL SHADOW", &self.os);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt;
        let spt = pt * 0.86;
        let lh = f.line_h(pt);
        let slh = f.line_h(spt);
        self.hits.clear();
        self.back = None;
        let x0 = pad + f.px(10.0);
        let inner = w - 2 * x0;
        match self.page {
            Page::Choose => {
                f.text(x0, top + f.px(4.0), "How much of Shadow do you want?", pt * 1.15, th.text, true);
                let cy = top + lh * 2 + f.px(8.0);
                let gap = f.px(18.0);
                let cw = (inner - gap) / 2;
                let tlh0 = f.line_h(pt * 1.3);
                let ch = (f.px(56.0) + tlh0 + slh * 2 + 7 * (slh + f.px(3.0))).min(h - cy - f.px(80.0));
                let cards: [(&str, &str, &[&str]); 2] = [
                    ("FULL DESKTOP", "Shadow becomes your desktop", &[
                        "glyph desktop, taskbar, widgets",
                        "every Shadow app and the launcher",
                        "power daemon · Wi-Fi self-repair",
                        "updates, crash reports, settings",
                        "its own compositor: a new",
                        "login session · auto updates",
                        "needs your password (sudo)",
                    ]),
                    ("APPS ONLY", "keep your desktop, add the apps", &[
                        "pick the apps you want",
                        "installed for your user (~/.local)",
                        "no session, no login screen,",
                        "no system services changed",
                        "they show up in your app menu",
                        "remove: ./install.sh --remove-apps",
                        "sudo only for build tools",
                    ]),
                ];
                for (i, (title, sub, lines)) in cards.iter().enumerate() {
                    let x = x0 + i as i32 * (cw + gap);
                    let accent = if self.sel == i { th.amber } else { th.cyan };
                    panel(f, x, cy, cw, ch, accent);
                    if self.sel == i {
                        f.cv.glow(x, cy, cw, ch, 2.0, f.px(10.0), th.amber, 0.18);
                    }
                    let tx = x + f.px(22.0);
                    f.text(tx, cy + f.px(18.0), title, pt * 1.3, if self.sel == i { th.amber } else { th.text }, true);
                    let tlh = f.line_h(pt * 1.3);
                    f.text(tx, cy + f.px(18.0) + tlh, sub, spt, th.cyan, false);
                    let mut y = cy + f.px(30.0) + tlh + slh + f.px(8.0);
                    for l in lines.iter() {
                        let l = f.fit(&format!("· {l}"), spt, false, cw - f.px(40.0));
                        f.text(tx, y, &l, spt, th.dim, false);
                        y += slh + f.px(3.0);
                    }
                    self.hits.push((i, Rect { x, y: cy, w: cw, h: ch }));
                }
                footer(f, "← → choose · Enter continue · Esc cancel · or click a card");
            }
            Page::Full => {
                f.text(x0, top + f.px(4.0), "FULL DESKTOP — options", pt * 1.15, th.amber, true);
                f.text(x0, top + f.px(4.0) + lh + f.px(6.0), "Everything else is always included. The installer runs in your terminal and asks for your password there.", spt, th.dim, false);
                let mut y = top + lh * 2 + slh + f.px(24.0);
                let opts = [
                    ("Login screen", "greetd with Shadow's greeter; your current login manager is switched off (reversible)", self.greeter),
                    ("Security package", "optional hardening modules — nothing is active until you enable each one", self.sec),
                ];
                let row_h = lh + slh + f.px(16.0);
                for (i, (name, desc, on)) in opts.iter().enumerate() {
                    let r = Rect { x: x0, y, w: inner, h: row_h - f.px(4.0) };
                    if self.sel == i {
                        selection(f, r.x, r.y, r.w, r.h);
                    }
                    let mark = if *on { "[■]" } else { "[ ]" };
                    f.text(x0 + f.px(18.0), y + f.px(6.0), mark, pt, if *on { th.green } else { th.dim }, true);
                    let mx = x0 + f.px(18.0) + f.text_w("[■]", pt, true) + f.px(12.0);
                    f.text(mx, y + f.px(6.0), name, pt, th.text, self.sel == i);
                    let d = f.fit(desc, spt, false, inner - (mx - x0) - f.px(10.0));
                    f.text(mx, y + f.px(6.0) + lh, &d, spt, th.dim, false);
                    self.hits.push((i, r));
                    y += row_h;
                }
                let r = button(f, x0, y + f.px(18.0), "INSTALL THE FULL DESKTOP", self.sel == 2, true);
                self.hits.push((2, r));
                let b = button(f, r.x + r.w + f.px(14.0), y + f.px(18.0), "BACK", false, true);
                self.back = Some(b);
                footer(f, "↑↓ choose · Space tick · Enter install · Esc back");
            }
            Page::Apps => {
                f.text(x0, top + f.px(4.0), "APPS ONLY — tick what you want", pt * 1.15, th.amber, true);
                f.text(x0, top + f.px(4.0) + lh + f.px(6.0), "Installed for your user in ~/.local — your desktop and system stay as they are.", spt, th.dim, false);
                let mut y = top + lh * 2 + slh + f.px(22.0);
                let row_h = lh + slh + f.px(12.0);
                for (i, (_, name, desc, note)) in APPS.iter().enumerate() {
                    let r = Rect { x: x0, y, w: inner, h: row_h - f.px(4.0) };
                    if self.sel == i {
                        selection(f, r.x, r.y, r.w, r.h);
                    }
                    let on = self.picked[i];
                    f.text(x0 + f.px(18.0), y + f.px(4.0), if on { "[■]" } else { "[ ]" }, pt, if on { th.green } else { th.dim }, true);
                    let mx = x0 + f.px(18.0) + f.text_w("[■]", pt, true) + f.px(12.0);
                    f.text(mx, y + f.px(4.0), name, pt, th.text, self.sel == i);
                    if !note.is_empty() {
                        let nw = f.text_w(note, spt, false).min(inner / 2);
                        let n = f.fit(note, spt, false, inner / 2);
                        f.text(x0 + inner - nw - f.px(12.0), y + f.px(6.0), &n, spt, th.amber, false);
                    }
                    let d = f.fit(desc, spt, false, inner - (mx - x0) - f.px(10.0));
                    f.text(mx, y + f.px(4.0) + lh, &d, spt, th.dim, false);
                    self.hits.push((i, r));
                    y += row_h;
                }
                let n = self.picked.iter().filter(|p| **p).count();
                let label = format!("INSTALL {n} APP{}", if n == 1 { "" } else { "S" });
                let r = button(f, x0, y + f.px(14.0), &label, self.sel == APPS.len(), n > 0);
                self.hits.push((APPS.len(), r));
                let b = button(f, r.x + r.w + f.px(14.0), y + f.px(14.0), "BACK", false, true);
                self.back = Some(b);
                if !self.msg.is_empty() {
                    f.text(b.x + b.w + f.px(18.0), y + f.px(20.0), &self.msg, spt, th.crimson, false);
                }
                footer(f, "↑↓ choose · Space tick · a all · Enter install · Esc back");
            }
        }
        let _ = h;
    }
}

fn main() {
    let os = std::fs::read_to_string("/etc/os-release").ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))).unwrap_or_default();
    let start_apps = std::env::args().any(|a| a == "--apps");
    let app = Installer {
        page: if start_apps { Page::Apps } else { Page::Choose }, sel: 0, picked: APPS.iter().map(|a| a.0 != "shadow-wallpaper").collect(), greeter: false, sec: false,
        os, hits: vec![], back: None, msg: String::new(),
    };
    if let Err(e) = run(Box::new(app), None) {
        eprintln!("shadow-installer: {e}");
        std::process::exit(1);
    }
}
