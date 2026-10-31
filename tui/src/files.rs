//! sakura files — keyboard-first file manager (yazi-like, three panes), std-only.
//!
//!   ↑↓ / j k      move                 ← / h / Backspace   parent directory
//!   → / l / Enter open (dir: enter, text: $EDITOR, other: default app)
//!   space         select               a           select all / none
//!   y  x  p       copy, cut, paste     d           move to Trash (freedesktop)
//!   D             delete permanently   r           rename
//!   n             new file (end with / for a folder)
//!   o             open with the default app          e   edit with $EDITOR
//!   /             filter               .           hidden files
//!   s             sort: name / modified / size        ~   home   `   previous dir
//!   !             run a command here   S           shell here
//!   q / Esc       quit (prints the directory with --print-cwd, for shell `cd`)
//! Mouse: click to select, click again to open, wheel to scroll, click the left pane to go up.
use crate::common::{active_theme, home, read_json, settings_path, short, Ui};
use crate::json::Value;
use crate::term::{Input, Key, Style, Term};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    pub link: bool,
    pub size: u64,
    pub mtime: u64,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Sort {
    Name,
    Modified,
    Size,
}

enum Prompt {
    Rename(PathBuf),
    New,
    Command,
}

enum Confirm {
    Trash(Vec<PathBuf>),
    Delete(Vec<PathBuf>),
}

pub fn list(dir: &Path, hidden: bool, sort: Sort) -> Vec<Entry> {
    let mut v: Vec<Entry> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !hidden && name.starts_with('.') {
                        return None;
                    }
                    let path = e.path();
                    let lmeta = std::fs::symlink_metadata(&path).ok()?;
                    let meta = std::fs::metadata(&path).unwrap_or(lmeta.clone());
                    Some(Entry {
                        name,
                        dir: meta.is_dir(),
                        link: lmeta.file_type().is_symlink(),
                        size: meta.len(),
                        mtime: meta.modified().ok().and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()),
                        path,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| {
        b.dir.cmp(&a.dir).then_with(|| match sort {
            Sort::Name => natural(&a.name, &b.name),
            Sort::Modified => b.mtime.cmp(&a.mtime),
            Sort::Size => b.size.cmp(&a.size),
        })
    });
    v
}

/// Case-insensitive natural order: "file2" before "file10".
pub fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n1 = String::new();
                while let Some(c) = x.peek().copied().filter(|c| c.is_ascii_digit()) {
                    n1.push(c);
                    x.next();
                }
                let mut n2 = String::new();
                while let Some(c) = y.peek().copied().filter(|c| c.is_ascii_digit()) {
                    n2.push(c);
                    y.next();
                }
                let o = n1.trim_start_matches('0').len().cmp(&n2.trim_start_matches('0').len())
                    .then_with(|| n1.trim_start_matches('0').cmp(n2.trim_start_matches('0')));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

pub fn human(n: u64) -> String {
    let u = ["B", "K", "M", "G", "T"];
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < u.len() - 1 {
        f /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n}B") } else { format!("{f:.1}{}", u[i]) }
}

fn is_text(p: &Path) -> bool {
    use std::io::Read;
    let mut buf = [0u8; 4096];
    let Ok(mut f) = std::fs::File::open(p) else { return false };
    let n = f.read(&mut buf).unwrap_or(0);
    !buf[..n].contains(&0) && std::str::from_utf8(&buf[..n]).is_ok() || n == 0
}

fn png_size(p: &Path) -> Option<(u32, u32)> {
    let b = std::fs::read(p).ok()?;
    if b.len() > 24 && &b[..8] == b"\x89PNG\r\n\x1a\n" {
        let w = u32::from_be_bytes(b[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(b[20..24].try_into().ok()?);
        return Some((w, h));
    }
    None
}

fn preview(e: &Entry, hidden: bool, max: usize) -> Vec<String> {
    if e.dir {
        let l = list(&e.path, hidden, Sort::Name);
        if l.is_empty() {
            return vec!["(empty)".into()];
        }
        return l.iter().take(max).map(|x| if x.dir { format!("{}/", x.name) } else { x.name.clone() }).collect();
    }
    if e.size <= 2 * 1024 * 1024 && is_text(&e.path) {
        let text = std::fs::read_to_string(&e.path).unwrap_or_default();
        return text.lines().take(max).map(|l| l.replace('\t', "    ").chars().filter(|c| !c.is_control()).collect()).collect();
    }
    let mut v = vec![format!("{}  ·  {}", human(e.size), ext_kind(&e.name))];
    if let Some((w, h)) = png_size(&e.path) {
        v.push(format!("PNG image {w}×{h}"));
    }
    v.push(String::new());
    v.push("o: open with the default app".into());
    v
}

fn ext_kind(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "avif" => "image",
        "mp4" | "mkv" | "webm" | "mov" => "video",
        "mp3" | "flac" | "ogg" | "wav" | "opus" => "audio",
        "pdf" => "PDF document",
        "zip" | "tar" | "gz" | "xz" | "zst" | "7z" => "archive",
        "docx" | "odt" | "doc" => "document",
        "xlsx" | "ods" | "csv" => "spreadsheet",
        _ => "file",
    }
}

// ---------------------------------------------------------------- file operations
pub fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(src)?;
        return std::os::unix::fs::symlink(target, dst);
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_recursive(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// "name", "name (2)", "name (3)"… so pasting never overwrites.
pub fn free_name(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if std::fs::symlink_metadata(&p).is_err() {
        return p;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| std::fs::symlink_metadata(p).is_err()).unwrap()
}

pub fn move_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        // across filesystems: copy, then remove
        Err(e) if e.raw_os_error() == Some(18) => {
            copy_recursive(src, dst)?;
            if src.is_dir() { std::fs::remove_dir_all(src) } else { std::fs::remove_file(src) }
        }
        Err(e) => Err(e),
    }
}

fn url_encode(p: &str) -> String {
    p.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// freedesktop.org Trash (~/.local/share/Trash), so file managers and `gio trash --restore` see it.
pub fn trash(p: &Path) -> std::io::Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("Trash");
    std::fs::create_dir_all(base.join("files"))?;
    std::fs::create_dir_all(base.join("info"))?;
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let dst = free_name(&base.join("files"), &name);
    let dname = dst.file_name().unwrap().to_string_lossy().into_owned();
    let abs = std::fs::canonicalize(p.parent().unwrap_or(Path::new("/"))).unwrap_or_default().join(p.file_name().unwrap_or_default());
    let date = crate::common::cmd_out(&["date", "+%Y-%m-%dT%H:%M:%S"]).1;
    std::fs::write(base.join("info").join(format!("{dname}.trashinfo")),
        format!("[Trash Info]\nPath={}\nDeletionDate={}\n", url_encode(&abs.to_string_lossy()), date.trim()))?;
    move_path(p, &dst)?;
    Ok(dst)
}

/// Run a command with the terminal handed over (editor, shell); returns the new Term.
pub fn suspended(t: Term, cmd: &[String], dir: &Path, pause: bool) -> Option<Term> {
    let mut t = t;
    t.leave();
    drop(t);
    let code = std::process::Command::new(&cmd[0]).args(&cmd[1..]).current_dir(dir).status().map(|s| s.code().unwrap_or(1)).unwrap_or(127);
    if pause {
        print!("\n[exit {code}] Enter to return… ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut l = String::new();
        let _ = std::io::stdin().read_line(&mut l);
    }
    Term::enter(None).ok()
}

fn editor() -> String {
    std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| {
        ["nvim", "vim", "nano", "vi"].iter().find(|e| crate::common::cmd_out(&["sh", "-c", &format!("command -v {e}")]).0 == 0).unwrap_or(&"vi").to_string()
    })
}

fn open_default(p: &Path) {
    let _ = std::process::Command::new("setsid")
        .args(["-f", "xdg-open"])
        .arg(p)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

struct App {
    cwd: PathBuf,
    prev: Option<PathBuf>,
    entries: Vec<Entry>,
    sel: usize,
    scroll: usize,
    hidden: bool,
    sort: Sort,
    marked: HashSet<PathBuf>,
    clip: Option<(Vec<PathBuf>, bool)>,
    filter: String,
    filtering: Option<Input>,
    prompt: Option<(Input, Prompt)>,
    confirm: Option<Confirm>,
    msg: (String, bool),
    ui: Ui,
    last_click: Option<(usize, Instant)>,
    quit: bool,
}

impl App {
    fn visible(&self) -> Vec<&Entry> {
        let f = self.filter.to_lowercase();
        self.entries.iter().filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f)).collect()
    }

    fn current(&self) -> Option<Entry> {
        self.visible().get(self.sel).map(|e| (*e).clone())
    }

    fn reload(&mut self) {
        let keep = self.current().map(|e| e.name);
        self.entries = list(&self.cwd, self.hidden, self.sort);
        if let Some(k) = keep {
            if let Some(i) = self.visible().iter().position(|e| e.name == k) {
                self.sel = i;
            }
        }
        self.sel = self.sel.min(self.visible().len().saturating_sub(1));
    }

    fn cd(&mut self, dir: PathBuf, focus: Option<String>) {
        if std::fs::read_dir(&dir).is_err() {
            self.msg = (format!("cannot open {}", short(&dir.to_string_lossy())), true);
            return;
        }
        self.prev = Some(std::mem::replace(&mut self.cwd, dir));
        self.filter.clear();
        self.entries = list(&self.cwd, self.hidden, self.sort);
        self.sel = focus.and_then(|f| self.visible().iter().position(|e| e.name == f)).unwrap_or(0);
        self.scroll = 0;
    }

    fn targets(&self) -> Vec<PathBuf> {
        if self.marked.is_empty() {
            self.current().map(|e| vec![e.path]).unwrap_or_default()
        } else {
            let mut v: Vec<PathBuf> = self.marked.iter().cloned().collect();
            v.sort();
            v
        }
    }

    fn paste(&mut self) {
        let Some((paths, cut)) = self.clip.clone() else {
            self.msg = ("nothing to paste — y copies, x cuts".into(), true);
            return;
        };
        let (mut ok, mut errs) = (0, vec![]);
        for p in &paths {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if self.cwd.starts_with(p) {
                errs.push(format!("{name}: cannot paste a folder into itself"));
                continue;
            }
            let dst = free_name(&self.cwd, &name);
            let r = if cut { move_path(p, &dst) } else { copy_recursive(p, &dst) };
            match r {
                Ok(()) => ok += 1,
                Err(e) => errs.push(format!("{name}: {e}")),
            }
        }
        if cut {
            self.clip = None;
        }
        self.marked.clear();
        self.reload();
        self.msg = if errs.is_empty() {
            (format!("{} {ok} item(s)", if cut { "moved" } else { "copied" }), false)
        } else {
            (errs.join("; "), true)
        };
    }

    fn open(&mut self, t: Term, e: &Entry, force_default: bool, force_edit: bool) -> Option<Term> {
        if e.dir && !force_default {
            self.cd(e.path.clone(), None);
            return Some(t);
        }
        if force_edit || (!force_default && is_text(&e.path)) {
            let ed = editor();
            let mut cmd: Vec<String> = ed.split_whitespace().map(String::from).collect();
            cmd.push(e.path.to_string_lossy().into_owned());
            let t = suspended(t, &cmd, &self.cwd, false);
            self.reload();
            return t;
        }
        open_default(&e.path);
        self.msg = (format!("opened {}", e.name), false);
        Some(t)
    }

    fn draw(&mut self, t: &mut Term) {
        let u = &self.ui;
        let (w, h) = (t.w, t.h);
        t.clear(u.base);
        // header: path + clipboard + sort
        let head = format!(" ❀ dot//sakura ▸ FILES  {}", short(&self.cwd.to_string_lossy()));
        t.fill(0, 0, w, 1, u.sel);
        t.text(0, 0, &head, u.sel, w);
        let info = format!(
            "{}{}{} ",
            if self.hidden { "hidden · " } else { "" },
            match self.sort { Sort::Name => "name", Sort::Modified => "modified", Sort::Size => "size" },
            self.clip.as_ref().map(|(c, cut)| format!(" · {} {}", c.len(), if *cut { "cut" } else { "copied" })).unwrap_or_default()
        );
        t.text(w.saturating_sub(info.chars().count()), 0, &info, u.sel, w);
        let body = h.saturating_sub(3);
        let (pw, cw) = (w * 2 / 10, w * 4 / 10);
        let px = 0;
        let cx = pw + 1;
        let vx = pw + cw + 2;
        let line = Style::new(u.theme.c("inactive"), u.theme.c("bg"));
        for y in 1..h - 2 {
            t.put(pw, y, '│', line);
            t.put(pw + cw + 1, y, '│', line);
        }
        // parent pane
        if let Some(parent) = self.cwd.parent() {
            let me = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
            for (i, e) in list(parent, self.hidden, Sort::Name).iter().take(body).enumerate() {
                let here = Some(&e.name) == me.as_ref();
                let st = if here { Style { bg: u.theme.c("bg_alt"), ..u.accent } } else if e.dir { u.accent2 } else { u.dim };
                t.text(px + 1, 1 + i, &e.name, st, pw.saturating_sub(2));
            }
        }
        // current pane
        let vis: Vec<Entry> = self.visible().into_iter().cloned().collect();
        if self.sel < self.scroll {
            self.scroll = self.sel;
        }
        if self.sel >= self.scroll + body {
            self.scroll = self.sel + 1 - body;
        }
        for (i, e) in vis.iter().enumerate().skip(self.scroll).take(body) {
            let y = 1 + i - self.scroll;
            let selected = i == self.sel;
            let marked = self.marked.contains(&e.path);
            let base = if e.dir { u.accent2 } else if e.link { u.ok } else { u.base };
            let st = if selected { u.sel } else { base };
            if selected {
                t.fill(cx, y, cw, 1, u.sel);
            }
            let mark = if marked { '▌' } else { ' ' };
            t.put(cx, y, mark, if selected { u.sel } else { u.alert });
            let name = format!("{}{}", e.name, if e.dir { "/" } else if e.link { "@" } else { "" });
            let size = if e.dir { String::new() } else { human(e.size) };
            let room = cw.saturating_sub(size.len() + 3);
            t.text(cx + 1, y, &name, st, room);
            t.text(cx + cw - size.len() - 1, y, &size, if selected { u.sel } else { u.dim }, size.len());
        }
        if vis.is_empty() {
            t.text(cx + 1, 1, if self.filter.is_empty() { "(empty)" } else { "(no match)" }, u.dim, cw);
        }
        // preview pane
        if let Some(e) = vis.get(self.sel) {
            for (i, l) in preview(e, self.hidden, body).iter().enumerate() {
                t.text(vx + 1, 1 + i, l, if e.dir { u.dim } else { u.base }, w.saturating_sub(vx + 2));
            }
        }
        // status line
        let st_y = h - 2;
        let status = if let Some(c) = &self.confirm {
            let (what, n) = match c { Confirm::Trash(v) => ("Move to Trash", v.len()), Confirm::Delete(v) => ("DELETE PERMANENTLY", v.len()) };
            (format!(" {what} {n} item(s)? y / N"), true)
        } else {
            self.msg.clone()
        };
        t.fill(0, st_y, w, 1, u.panel);
        if let Some(e) = vis.get(self.sel) {
            let meta = format!(" {}/{} · {} ", self.sel + 1, vis.len(), if e.dir { "folder".into() } else { human(e.size) });
            t.text(w.saturating_sub(meta.len()), st_y, &meta, u.panel, w);
        }
        t.text(1, st_y, &status.0, if status.1 { Style { bg: u.theme.c("bg_alt"), ..u.alert } } else { u.panel }, w.saturating_sub(20));
        // bottom: input or key help
        let by = h - 1;
        if let Some((inp, p)) = &self.prompt {
            let label = match p { Prompt::Rename(_) => "rename ▸ ", Prompt::New => "new (end with / for folder) ▸ ", Prompt::Command => "run here ▸ " };
            t.text(0, by, label, u.accent, w);
            inp.draw(t, label.chars().count(), by, w.saturating_sub(label.chars().count() + 1), u.input, true, None);
        } else if let Some(inp) = &self.filtering {
            t.text(0, by, "filter ▸ ", u.accent, w);
            inp.draw(t, 9, by, w.saturating_sub(10), u.input, true, None);
        } else {
            t.text(0, by, " enter open · space select · y copy · x cut · p paste · d trash · r rename · n new · / filter · . hidden · s sort · ! cmd · q quit", u.dim, w);
        }
    }
}

pub fn run(args: &[String]) -> i32 {
    let print_cwd = args.iter().any(|a| a == "--print-cwd");
    let start = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok()).unwrap_or_else(home);
    let start = std::fs::canonicalize(&start).unwrap_or(start);
    let settings = read_json(&settings_path()).unwrap_or(Value::Obj(vec![]));
    let ui = Ui::new(&active_theme(&settings), false);
    let Ok(mut t) = Term::enter(None) else {
        eprintln!("sakura files: needs a terminal");
        return 1;
    };
    t.mouse(true);
    let mut a = App {
        cwd: start.clone(),
        prev: None,
        entries: vec![],
        sel: 0,
        scroll: 0,
        hidden: false,
        sort: Sort::Name,
        marked: HashSet::new(),
        clip: None,
        filter: String::new(),
        filtering: None,
        prompt: None,
        confirm: None,
        msg: (String::new(), false),
        ui,
        last_click: None,
        quit: false,
    };
    if start.is_file() {
        let name = start.file_name().map(|n| n.to_string_lossy().into_owned());
        a.cwd = start.parent().map(Path::to_path_buf).unwrap_or_else(home);
        a.cd(a.cwd.clone(), name);
    } else {
        a.reload();
    }
    let mut term = Some(t);
    while !a.quit {
        let Some(mut t) = term.take() else { return 1 };
        if t.resize() {
            t.force_redraw();
        }
        a.draw(&mut t);
        let _ = t.present();
        let mut keys = t.keys();
        let mut t = Some(t);
        for k in keys.drain(..) {
            let Some(tt) = t.take() else { break };
            t = handle(&mut a, tt, k);
            if a.quit {
                break;
            }
        }
        term = t;
        if term.is_none() {
            return 1;
        }
        if let Some(t) = term.as_mut() {
            t.mouse(true);
        }
    }
    if let Some(mut t) = term {
        t.leave();
    }
    if print_cwd {
        println!("{}", a.cwd.display());
    }
    0
}

fn handle(a: &mut App, mut t: Term, k: Key) -> Option<Term> {
    let len = a.visible().len();
    if let Some(c) = a.confirm.take() {
        if k == Key::Char('y') || k == Key::Char('Y') {
            let (mut n, mut errs) = (0, vec![]);
            match &c {
                Confirm::Trash(v) => v.iter().for_each(|p| match trash(p) { Ok(_) => n += 1, Err(e) => errs.push(e.to_string()) }),
                Confirm::Delete(v) => v.iter().for_each(|p| {
                    let r = if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
                    match r { Ok(()) => n += 1, Err(e) => errs.push(e.to_string()) }
                }),
            }
            a.marked.clear();
            a.reload();
            a.msg = if errs.is_empty() { (format!("{n} item(s) {}", if matches!(c, Confirm::Trash(_)) { "moved to Trash" } else { "deleted" }), false) } else { (errs.join("; "), true) };
        } else {
            a.msg = ("cancelled".into(), false);
        }
        return Some(t);
    }
    if let Some((mut inp, p)) = a.prompt.take() {
        match k {
            Key::Esc => a.msg = ("cancelled".into(), false),
            Key::Enter => {
                let v = inp.buf.trim().to_string();
                if v.is_empty() {
                    return Some(t);
                }
                match p {
                    Prompt::Rename(from) => {
                        if v.contains('/') {
                            a.msg = ("a name cannot contain /".into(), true);
                        } else {
                            let to = a.cwd.join(&v);
                            if std::fs::symlink_metadata(&to).is_ok() {
                                a.msg = (format!("{v} already exists"), true);
                            } else {
                                match std::fs::rename(&from, &to) {
                                    Ok(()) => { a.reload(); a.sel = a.visible().iter().position(|e| e.name == v).unwrap_or(a.sel); a.msg = (format!("renamed to {v}"), false) }
                                    Err(e) => a.msg = (e.to_string(), true),
                                }
                            }
                        }
                    }
                    Prompt::New => {
                        let p = a.cwd.join(v.trim_end_matches('/'));
                        let r = if v.ends_with('/') { std::fs::create_dir_all(&p) } else if p.exists() {
                            Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "already exists"))
                        } else {
                            if let Some(par) = p.parent() { let _ = std::fs::create_dir_all(par); }
                            std::fs::File::create(&p).map(|_| ())
                        };
                        match r {
                            Ok(()) => { a.reload(); let n = p.file_name().unwrap().to_string_lossy().into_owned(); a.sel = a.visible().iter().position(|e| e.name == n).unwrap_or(a.sel); a.msg = ("created".into(), false) }
                            Err(e) => a.msg = (e.to_string(), true),
                        }
                    }
                    Prompt::Command => {
                        let cmd = vec!["sh".to_string(), "-c".into(), v];
                        let r = suspended(t, &cmd, &a.cwd.clone(), true);
                        a.reload();
                        return r;
                    }
                }
            }
            _ => {
                inp.handle(k);
                a.prompt = Some((inp, p));
            }
        }
        return Some(t);
    }
    if let Some(mut inp) = a.filtering.take() {
        match k {
            Key::Esc => a.filter.clear(),
            Key::Enter => {}
            _ => {
                inp.handle(k);
                a.filter = inp.buf.clone();
                a.sel = 0;
                a.filtering = Some(inp);
            }
        }
        return Some(t);
    }
    a.msg = (String::new(), false);
    match k {
        Key::Char('q') | Key::Esc | Key::Ctrl('c') => a.quit = true,
        Key::Up | Key::Char('k') => a.sel = a.sel.saturating_sub(1),
        Key::Down | Key::Char('j') => a.sel = (a.sel + 1).min(len.saturating_sub(1)),
        Key::PageUp => a.sel = a.sel.saturating_sub(t.h / 2),
        Key::PageDown => a.sel = (a.sel + t.h / 2).min(len.saturating_sub(1)),
        Key::Home | Key::Char('g') => a.sel = 0,
        Key::End | Key::Char('G') => a.sel = len.saturating_sub(1),
        Key::Left | Key::Char('h') | Key::Backspace => {
            if let Some(p) = a.cwd.parent().map(Path::to_path_buf) {
                let me = a.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
                a.cd(p, me);
            }
        }
        Key::Right | Key::Char('l') | Key::Enter => {
            if let Some(e) = a.current() {
                return a.open(t, &e, false, false);
            }
        }
        Key::Char('o') => if let Some(e) = a.current() { return a.open(t, &e, true, false) },
        Key::Char('e') => if let Some(e) = a.current() { if !e.dir { return a.open(t, &e, false, true) } },
        Key::Char(' ') => {
            if let Some(e) = a.current() {
                if !a.marked.remove(&e.path) {
                    a.marked.insert(e.path);
                }
                a.sel = (a.sel + 1).min(len.saturating_sub(1));
            }
        }
        Key::Char('a') => {
            if a.marked.is_empty() { a.marked = a.visible().iter().map(|e| e.path.clone()).collect() } else { a.marked.clear() }
        }
        Key::Char('y') | Key::Char('x') => {
            let v = a.targets();
            if !v.is_empty() {
                a.msg = (format!("{} item(s) {} — p to paste", v.len(), if k == Key::Char('x') { "cut" } else { "copied" }), false);
                a.clip = Some((v, k == Key::Char('x')));
                a.marked.clear();
            }
        }
        Key::Char('p') => a.paste(),
        Key::Char('d') | Key::Delete => { let v = a.targets(); if !v.is_empty() { a.confirm = Some(Confirm::Trash(v)) } }
        Key::Char('D') => { let v = a.targets(); if !v.is_empty() { a.confirm = Some(Confirm::Delete(v)) } }
        Key::Char('r') => if let Some(e) = a.current() { a.prompt = Some((Input::with(&e.name), Prompt::Rename(e.path))) },
        Key::Char('n') => a.prompt = Some((Input::default(), Prompt::New)),
        Key::Char('!') => a.prompt = Some((Input::default(), Prompt::Command)),
        Key::Char('S') => {
            let sh = std::env::var("SHELL").unwrap_or_else(|_| "bash".into());
            let r = suspended(t, &[sh], &a.cwd.clone(), false);
            a.reload();
            return r;
        }
        Key::Char('/') => a.filtering = Some(Input::with(&a.filter)),
        Key::Char('.') => { a.hidden = !a.hidden; a.reload() }
        Key::Char('s') => {
            a.sort = match a.sort { Sort::Name => Sort::Modified, Sort::Modified => Sort::Size, Sort::Size => Sort::Name };
            a.reload();
        }
        Key::Char('~') => a.cd(home(), None),
        Key::Char('`') => if let Some(p) = a.prev.clone() { a.cd(p, None) },
        Key::Ctrl('r') => a.reload(),
        Key::Mouse(btn, x, y, true) => {
            let (pw, cw) = (t.w * 2 / 10, t.w * 4 / 10);
            match btn {
                64 => a.sel = a.sel.saturating_sub(3),
                65 => a.sel = (a.sel + 3).min(len.saturating_sub(1)),
                0 if x < pw => {
                    if let Some(p) = a.cwd.parent().map(Path::to_path_buf) {
                        let me = a.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
                        a.cd(p, me);
                    }
                }
                0 if x <= pw + cw && y >= 1 && y < t.h - 2 => {
                    let i = a.scroll + y - 1;
                    if i < len {
                        let double = a.last_click.map_or(false, |(j, at)| j == i && at.elapsed().as_millis() < 500);
                        a.sel = i;
                        a.last_click = Some((i, Instant::now()));
                        if double {
                            if let Some(e) = a.current() {
                                return a.open(t, &e, false, false);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    t.mouse(true);
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn natural_order() {
        let mut v = vec!["file10", "file2", "File1", "file02b"];
        v.sort_by(|a, b| natural(a, b));
        assert_eq!(v, vec!["File1", "file2", "file02b", "file10"]);
    }
    #[test]
    fn ops_roundtrip() {
        let d = std::env::temp_dir().join(format!("sakura-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/sub")).unwrap();
        std::fs::write(d.join("src/a.txt"), "hello").unwrap();
        std::fs::write(d.join("src/sub/b.txt"), "x").unwrap();
        std::fs::create_dir_all(d.join("dst")).unwrap();
        copy_recursive(&d.join("src"), &d.join("dst/src")).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("dst/src/sub/b.txt")).unwrap(), "x");
        assert_eq!(free_name(&d.join("src"), "a.txt"), d.join("src/a (2).txt"));
        std::env::set_var("XDG_DATA_HOME", d.join("data"));
        let t = trash(&d.join("src/a.txt")).unwrap();
        assert!(t.exists() && !d.join("src/a.txt").exists());
        let info = std::fs::read_to_string(d.join("data/Trash/info/a.txt.trashinfo")).unwrap();
        assert!(info.contains("Path=") && info.contains("/src/a.txt"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
