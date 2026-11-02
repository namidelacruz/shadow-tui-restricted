//! shadow-files — three-column file manager (parent · current · preview), keyboard only.
//! Opening a file starts its app in a new window, which niri puts at center stage.
//!
//!   j/k move · h back · l/Enter open · e edit · o open with default · t terminal here
//!   Space mark · y copy · x cut · p paste · d trash · r rename · n new (end with / for a folder)
//!   / filter · . hidden files · s sort · ~ home · g/G top/bottom · q close
use shadow_apps::fsops::{self, Entry, Sort};
use shadow_apps::{home, sh_quote, spawn};
use shadow_ui::widgets::{chrome, footer, selection, Input, ListNav};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

enum Prompt {
    Rename(PathBuf),
    New,
    Filter,
    Trash(Vec<PathBuf>),
}

struct Files {
    cwd: PathBuf,
    entries: Vec<Entry>,
    parent: Vec<Entry>,
    nav: ListNav,
    visible: usize,
    hidden: bool,
    sort: Sort,
    filter: String,
    marked: BTreeSet<PathBuf>,
    clip: Vec<PathBuf>,
    cut: bool,
    prompt: Option<(Input, Prompt)>,
    msg: (String, bool),
    preview: Vec<String>,
}

fn editor() -> String {
    std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| {
        ["nvim", "vim", "nano", "vi"].iter().find(|e| shadow_apps::cmd_out(&["sh", "-c", &format!("command -v {e}")]).0 == 0).unwrap_or(&"vi").to_string()
    })
}

impl Files {
    fn load(&mut self) {
        let mut v = fsops::list(&self.cwd, self.hidden, self.sort);
        if !self.filter.is_empty() {
            let q = self.filter.to_lowercase();
            v.retain(|e| e.name.to_lowercase().contains(&q));
        }
        self.entries = v;
        self.parent = self.cwd.parent().map(|p| fsops::list(p, self.hidden, self.sort)).unwrap_or_default();
        self.nav.clamp(self.entries.len(), self.visible);
        self.load_preview();
    }

    fn load_preview(&mut self) {
        self.preview = match self.entries.get(self.nav.sel) {
            Some(e) => fsops::preview(e, self.hidden, 80),
            None => vec![],
        };
    }

    fn cd(&mut self, p: PathBuf, select: Option<String>) {
        if std::fs::read_dir(&p).is_err() {
            self.msg = (format!("cannot open {}", p.display()), true);
            return;
        }
        self.cwd = p;
        self.filter.clear();
        self.nav = ListNav::default();
        self.load();
        if let Some(n) = select {
            if let Some(i) = self.entries.iter().position(|e| e.name == n) {
                self.nav.sel = i;
                self.nav.clamp(self.entries.len(), self.visible);
                self.load_preview();
            }
        }
    }

    fn targets(&self) -> Vec<PathBuf> {
        if self.marked.is_empty() {
            self.entries.get(self.nav.sel).map(|e| vec![e.path.clone()]).unwrap_or_default()
        } else {
            self.marked.iter().cloned().collect()
        }
    }

    fn open(&mut self, with_default: bool) {
        let Some(e) = self.entries.get(self.nav.sel).cloned() else { return };
        if e.dir && !with_default {
            self.cd(e.path, None);
            return;
        }
        let p = sh_quote(&e.path.to_string_lossy());
        if !with_default && fsops::is_text(&e.path) {
            spawn(&format!("cd {} && exec {}", sh_quote(&self.cwd.to_string_lossy()), shadow_apps::terminal_exec(&format!("{} {p}", editor()))));
        } else {
            spawn(&format!("xdg-open {p}"));
        }
        self.msg = (format!("opened {}", e.name), false);
    }

    fn paste(&mut self) {
        let mut n = 0;
        for src in std::mem::take(&mut self.clip) {
            let name = src.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let dst = fsops::free_name(&self.cwd, &name);
            let r = if self.cut { fsops::move_path(&src, &dst) } else { fsops::copy_recursive(&src, &dst) };
            match r {
                Ok(()) => n += 1,
                Err(e) => self.msg = (format!("{name}: {e}"), true),
            }
        }
        if n > 0 {
            self.msg = (format!("{} {n} item(s)", if self.cut { "moved" } else { "copied" }), false);
        }
        self.cut = false;
        self.marked.clear();
        self.load();
    }

    fn prompt_key(&mut self, k: &Key) {
        let Some((mut inp, p)) = self.prompt.take() else { return };
        match (k.sym, &p) {
            (Keysym::Escape, Prompt::Filter) => {
                self.filter.clear();
                self.load();
            }
            (Keysym::Escape, _) => self.msg = ("cancelled".into(), false),
            (Keysym::Return | Keysym::KP_Enter, _) => {
                let v = inp.text.trim().to_string();
                match p {
                    Prompt::Filter => {}
                    Prompt::Rename(from) => {
                        if !v.is_empty() && !v.contains('/') {
                            let to = from.with_file_name(&v);
                            if to.exists() {
                                self.msg = (format!("{v} already exists"), true);
                            } else if let Err(e) = std::fs::rename(&from, &to) {
                                self.msg = (e.to_string(), true);
                            } else {
                                self.load();
                                self.select(&v);
                            }
                        }
                    }
                    Prompt::New => {
                        if !v.is_empty() {
                            let dir = v.ends_with('/');
                            let name = v.trim_end_matches('/');
                            let p = self.cwd.join(name);
                            let r = if dir { std::fs::create_dir_all(&p) } else { std::fs::OpenOptions::new().write(true).create_new(true).open(&p).map(|_| ()) };
                            match r {
                                Ok(()) => {
                                    self.load();
                                    self.select(name);
                                }
                                Err(e) => self.msg = (e.to_string(), true),
                            }
                        }
                    }
                    Prompt::Trash(_) => {}
                }
            }
            (_, Prompt::Trash(items)) => {
                if k.typed() == Some("y") {
                    let mut n = 0;
                    for t in items {
                        match fsops::trash(t) {
                            Ok(_) => n += 1,
                            Err(e) => self.msg = (format!("{}: {e}", t.display()), true),
                        }
                    }
                    self.marked.clear();
                    self.msg = (format!("{n} item(s) moved to the trash"), false);
                    self.load();
                } else {
                    self.msg = ("kept".into(), false);
                }
            }
            _ => {
                inp.key(k);
                if let Prompt::Filter = p {
                    self.filter = inp.text.clone();
                    self.nav = ListNav::default();
                    self.load();
                }
                self.prompt = Some((inp, p));
            }
        }
    }

    fn select(&mut self, name: &str) {
        if let Some(i) = self.entries.iter().position(|e| e.name == name) {
            self.nav.sel = i;
            self.nav.clamp(self.entries.len(), self.visible);
            self.load_preview();
        }
    }
}

impl App for Files {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-files".into(), title: "Files".into(), size: (1100, 660) }
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        ctx.redraw();
        if self.prompt.is_some() {
            self.prompt_key(k);
            return;
        }
        self.msg.0.clear();
        let typed = k.typed().unwrap_or("").to_string();
        let before = self.nav.sel;
        if self.nav.key(k, self.entries.len(), self.visible, true) {
            if self.nav.sel != before {
                self.load_preview();
            }
            return;
        }
        match (k.sym, typed.as_str()) {
            (Keysym::Escape, _) | (_, "q") => ctx.quit(),
            (Keysym::Left | Keysym::BackSpace, _) | (_, "h") => {
                if let Some(p) = self.cwd.parent().map(Path::to_path_buf) {
                    let name = self.cwd.file_name().map(|s| s.to_string_lossy().into_owned());
                    self.cd(p, name);
                }
            }
            (Keysym::Right | Keysym::Return | Keysym::KP_Enter, _) | (_, "l") => self.open(false),
            (_, "o") => self.open(true),
            (_, "e") => {
                if let Some(e) = self.entries.get(self.nav.sel).filter(|e| !e.dir) {
                    spawn(&shadow_apps::terminal_exec(&format!("{} {}", editor(), sh_quote(&e.path.to_string_lossy()))));
                }
            }
            (_, "t") => spawn(&shadow_apps::terminal_here(&self.cwd.to_string_lossy())),
            (_, " ") => {
                if let Some(e) = self.entries.get(self.nav.sel) {
                    if !self.marked.remove(&e.path) {
                        self.marked.insert(e.path.clone());
                    }
                    self.nav.sel = (self.nav.sel + 1).min(self.entries.len().saturating_sub(1));
                    self.nav.clamp(self.entries.len(), self.visible);
                    self.load_preview();
                }
            }
            (_, "y") | (_, "x") => {
                self.clip = self.targets();
                self.cut = typed == "x";
                self.msg = (format!("{} {} item(s) — p to paste", if self.cut { "cut" } else { "copied" }, self.clip.len()), false);
            }
            (_, "p") => self.paste(),
            (Keysym::Delete, _) | (_, "d") => {
                let t = self.targets();
                if !t.is_empty() {
                    self.msg = (format!("move {} item(s) to the trash? y / n", t.len()), true);
                    self.prompt = Some((Input::default(), Prompt::Trash(t)));
                }
            }
            (_, "r") => {
                if let Some(e) = self.entries.get(self.nav.sel) {
                    let mut i = Input::default();
                    i.set(&e.name);
                    self.prompt = Some((i, Prompt::Rename(e.path.clone())));
                }
            }
            (_, "n") => self.prompt = Some((Input::default(), Prompt::New)),
            (_, "/") => {
                let mut i = Input::default();
                i.set(&self.filter);
                self.prompt = Some((i, Prompt::Filter));
            }
            (_, ".") => {
                self.hidden = !self.hidden;
                self.load();
            }
            (_, "s") => {
                self.sort = match self.sort {
                    Sort::Name => Sort::Modified,
                    Sort::Modified => Sort::Size,
                    Sort::Size => Sort::Name,
                };
                self.msg = (format!("sorted by {:?}", self.sort).to_lowercase(), false);
                self.load();
            }
            (_, "~") => self.cd(home(), None),
            _ => {}
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let home_s = home().to_string_lossy().into_owned();
        let cwd_s = self.cwd.to_string_lossy().replacen(&home_s, "~", 1);
        let status = format!("{cwd_s}{}", if self.filter.is_empty() { String::new() } else { format!("  /{}", self.filter) });
        let (top, pad) = chrome(f, "FILES", &status);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt;
        let spt = pt * 0.9;
        let lh = f.line_h(pt);
        let row_h = lh + f.px(4.0);
        let foot = f.line_h(spt) + pad + lh + f.px(16.0);
        let bottom = h - foot;
        self.visible = ((bottom - top) / row_h).max(1) as usize;
        self.nav.clamp(self.entries.len(), self.visible);
        let inner = w - 2 * pad;
        let c1 = pad;
        let c2 = pad + inner * 20 / 100;
        let c3 = pad + inner * 58 / 100;
        let hair = f.hair();
        for x in [c2 - f.px(8.0), c3 - f.px(8.0)] {
            f.cv.fill(x, top, hair, bottom - top, th.amber, 0.3);
        }
        // parent column
        let cur_name = self.cwd.file_name().map(|s| s.to_string_lossy().into_owned());
        let pvis = self.visible;
        let pidx = self.parent.iter().position(|e| Some(&e.name) == cur_name.as_ref()).unwrap_or(0);
        let pstart = pidx.saturating_sub(pvis / 2).min(self.parent.len().saturating_sub(pvis));
        for (n, e) in self.parent.iter().enumerate().skip(pstart).take(pvis) {
            let y = top + (n - pstart) as i32 * row_h;
            let is_cur = Some(&e.name) == cur_name.as_ref();
            if is_cur {
                let r = row_h as f32 / 2.0;
                f.cv.rrect(c1, y, c2 - c1 - f.px(14.0), row_h, r, th.amber, 0.10);
            }
            let name = if e.dir { format!("{}/", e.name) } else { e.name.clone() };
            let t = f.fit(&name, spt, false, c2 - c1 - f.px(20.0));
            f.text(c1 + f.px(4.0), y + f.px(2.0), &t, spt, if e.dir { th.amber } else { th.dim }, false);
        }
        // current column
        if self.entries.is_empty() {
            f.text(c2, top, "(empty)", pt, th.dim, false);
        }
        for (n, e) in self.entries.iter().enumerate().skip(self.nav.top).take(self.visible) {
            let y = top + (n - self.nav.top) as i32 * row_h;
            let sel = n == self.nav.sel;
            if sel {
                selection(f, c2, y, c3 - c2 - f.px(14.0), row_h);
            }
            let marked = self.marked.contains(&e.path);
            let x = c2 + f.px(10.0);
            if marked {
                f.text(x - f.px(8.0), y + f.px(2.0), "▌", pt, th.cyan, true);
            }
            let size = if e.dir { String::new() } else { fsops::human(e.size) };
            let sw = f.text_w(&size, spt, false);
            let name = if e.dir { format!("{}/", e.name) } else if e.link { format!("{} →", e.name) } else { e.name.clone() };
            let t = f.fit(&name, pt, false, c3 - x - sw - f.px(30.0));
            let col = if marked { th.cyan } else if e.dir { th.amber } else { th.text };
            f.text(x, y + f.px(2.0), &t, pt, col, e.dir);
            if !size.is_empty() {
                let sy = y + f.px(2.0) + (lh - f.line_h(spt)) / 2;
                let sx = c3 - f.px(20.0) - sw;
                f.text(sx, sy, &size, spt, th.dim, false);
            }
        }
        // preview column
        let slh = f.line_h(spt);
        let pmax = ((bottom - top) / slh).max(1) as usize;
        for (n, l) in self.preview.iter().take(pmax).enumerate() {
            let t = f.fit(l, spt, false, w - pad - c3);
            f.text(c3, top + n as i32 * slh, &t, spt, th.text, false);
        }
        // prompt / message line
        let my = bottom + f.px(6.0);
        match &self.prompt {
            Some((inp, p)) => {
                let label = match p {
                    Prompt::Rename(_) => "rename ▸ ",
                    Prompt::New => "new (end with / for a folder) ▸ ",
                    Prompt::Filter => "filter ▸ ",
                    Prompt::Trash(_) => "",
                };
                if label.is_empty() {
                    let m = self.msg.0.clone();
                    f.text(pad, my, &m, pt, th.crimson, true);
                } else {
                    let lw = f.text(pad, my, label, pt, th.amber, true);
                    inp.draw(f, pad + lw, my, pt, true);
                }
            }
            None => {
                let info = match self.entries.get(self.nav.sel) {
                    Some(e) => format!("{}/{} · {}{}", self.nav.sel + 1, self.entries.len(), if e.dir { "folder".into() } else { fsops::human(e.size) }, if self.clip.is_empty() { String::new() } else { format!(" · clipboard {}", self.clip.len()) }),
                    None => String::new(),
                };
                let (m, col) = if self.msg.0.is_empty() { (info, th.dim) } else { (self.msg.0.clone(), if self.msg.1 { th.crimson } else { th.cyan }) };
                let m = f.fit(&m, pt, false, w - 2 * pad);
                f.text(pad, my, &m, pt, col, false);
            }
        }
        footer(f, "j/k move · h back · l open · e edit · o default app · t terminal · Space mark · y/x/p copy cut paste · d trash · r rename · n new · / filter · . hidden · s sort · q close");
    }
}

fn main() {
    let start = std::env::args().nth(1).map(PathBuf::from).filter(|p| p.is_dir()).or_else(|| std::env::current_dir().ok()).unwrap_or_else(home);
    let mut fl = Files { cwd: start.clone(), entries: vec![], parent: vec![], nav: ListNav::default(), visible: 20, hidden: false, sort: Sort::Name, filter: String::new(), marked: BTreeSet::new(), clip: vec![], cut: false, prompt: None, msg: (String::new(), false), preview: vec![] };
    fl.cd(start, None);
    if let Err(e) = run(Box::new(fl), None) {
        eprintln!("shadow-files: {e}");
        std::process::exit(1);
    }
}
