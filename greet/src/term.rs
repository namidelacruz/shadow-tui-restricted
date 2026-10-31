//! Minimal terminal layer using only std + libc symbols that std already links.
//! Raw mode via termios FFI, key decoding, a diffed cell buffer, truecolor or
//! Linux-VT (16 programmable palette slots) output, and guaranteed restore on
//! exit, panic, SIGTERM and SIGHUP.
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Termios {
    iflag: u32,
    oflag: u32,
    cflag: u32,
    lflag: u32,
    line: u8,
    cc: [u8; 32],
    ispeed: u32,
    ospeed: u32,
}

#[repr(C)]
struct WinSize {
    rows: u16,
    cols: u16,
    xpix: u16,
    ypix: u16,
}

extern "C" {
    fn tcgetattr(fd: i32, t: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, act: i32, t: *const Termios) -> i32;
    fn ioctl(fd: i32, req: std::ffi::c_ulong, ...) -> i32;
    fn isatty(fd: i32) -> i32;
    fn signal(sig: i32, handler: usize) -> usize;
    fn write(fd: i32, buf: *const std::ffi::c_void, n: usize) -> isize;
    fn _exit(code: i32) -> !;
}

const TIOCGWINSZ: std::ffi::c_ulong = 0x5413;
// c_iflag
const BRKINT: u32 = 0o2;
const INPCK: u32 = 0o20;
const ISTRIP: u32 = 0o40;
const ICRNL: u32 = 0o400;
const IXON: u32 = 0o2000;
// c_oflag
const OPOST: u32 = 0o1;
// c_cflag
const CS8: u32 = 0o60;
// c_lflag
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const IEXTEN: u32 = 0o100000;
const VTIME: usize = 5;
const VMIN: usize = 6;

static ORIG: OnceLock<Termios> = OnceLock::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
static VT_PALETTE: AtomicBool = AtomicBool::new(false);
const LEAVE_SEQ: &[u8] = b"\x1b[?1006l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l";

/// Restore the terminal. Async-signal-safe (only write + tcsetattr).
fn restore_raw() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return;
    }
    unsafe {
        if VT_PALETTE.load(Ordering::SeqCst) {
            write(1, b"\x1b]R".as_ptr().cast(), 3);
        }
        write(1, LEAVE_SEQ.as_ptr().cast(), LEAVE_SEQ.len());
        if let Some(t) = ORIG.get() {
            tcsetattr(0, 0, t);
        }
    }
}

extern "C" fn on_signal(_sig: i32) {
    restore_raw();
    unsafe { _exit(130) }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Esc,
    Backspace,
    Delete,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    /// SGR mouse (after Term::mouse(true)): button (0 left, 1 middle, 2 right, 64/65 wheel up/down),
    /// column, row (0-based), pressed (false = release)
    Mouse(u8, usize, usize, bool),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Color {
    Default,
    Rgb(u8, u8, u8),
}

impl Color {
    pub fn hex(s: &str) -> Color {
        let s = s.trim_start_matches('#');
        if s.len() < 6 {
            return Color::Default;
        }
        let p = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0);
        Color::Rgb(p(0), p(2), p(4))
    }
    pub fn mix(self, other: Color, t: f32) -> Color {
        match (self, other) {
            (Color::Rgb(a, b, c), Color::Rgb(d, e, f)) => {
                let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
                Color::Rgb(m(a, d), m(b, e), m(c, f))
            }
            _ => self,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub underline: bool,
}

impl Style {
    pub fn new(fg: Color, bg: Color) -> Style {
        Style { fg, bg, bold: false, underline: false }
    }
    pub fn bold(mut self) -> Style {
        self.bold = true;
        self
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Cell {
    ch: char,
    st: Style,
}

pub struct Term {
    pub w: usize,
    pub h: usize,
    cells: Vec<Cell>,
    prev: Vec<Cell>,
    /// Linux VT: 16 palette slots programmed from the theme; colours are
    /// mapped to the nearest slot (bg limited to slots 0-7).
    vt_slots: Option<[Color; 16]>,
    pending: Vec<u8>,
    pub cursor: Option<(usize, usize)>,
}

impl Term {
    pub fn is_tty() -> bool {
        unsafe { isatty(0) == 1 && isatty(1) == 1 }
    }

    /// Linux virtual console: TERM=linux, or stdin is /dev/ttyN (TERM unset).
    pub fn linux_console() -> bool {
        if std::env::var("TERM").map(|t| t == "linux").unwrap_or(false) {
            return true;
        }
        let term_unset = std::env::var("TERM").map(|t| t.is_empty() || t == "dumb").unwrap_or(true);
        let tty = std::fs::read_link("/proc/self/fd/0").map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        term_unset && tty.strip_prefix("/dev/tty").map(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit())).unwrap_or(false)
    }

    /// Enter raw mode + alternate screen. `vt_palette` = 16 colours to program
    /// into a Linux console (ignored on other terminals).
    pub fn enter(vt_palette: Option<[Color; 16]>) -> io::Result<Term> {
        if !Term::is_tty() {
            return Err(io::Error::new(io::ErrorKind::Other, "not a terminal"));
        }
        let mut t: Termios = unsafe { std::mem::zeroed() };
        if unsafe { tcgetattr(0, &mut t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let _ = ORIG.set(t);
        let mut raw = t;
        raw.iflag &= !(BRKINT | ICRNL | INPCK | ISTRIP | IXON);
        raw.oflag &= !OPOST;
        raw.cflag |= CS8;
        raw.lflag &= !(ECHO | ICANON | ISIG | IEXTEN);
        raw.cc[VMIN] = 1;
        raw.cc[VTIME] = 0; // block until a key: a static screen costs no wakeups
        if unsafe { tcsetattr(0, 0, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        ACTIVE.store(true, Ordering::SeqCst);
        unsafe {
            signal(15, on_signal as *const () as usize); // SIGTERM
            signal(1, on_signal as *const () as usize); // SIGHUP
        }
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_raw();
            prev_hook(info);
        }));
        let mut out = io::stdout();
        out.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J")?;
        let vt_slots = if Term::linux_console() { vt_palette } else { None };
        if let Some(slots) = vt_slots {
            VT_PALETTE.store(true, Ordering::SeqCst);
            for (i, c) in slots.iter().enumerate() {
                if let Color::Rgb(r, g, b) = c {
                    write!(out, "\x1b]P{:x}{:02x}{:02x}{:02x}", i, r, g, b)?;
                }
            }
        }
        out.flush()?;
        let mut term = Term { w: 0, h: 0, cells: vec![], prev: vec![], vt_slots, pending: vec![], cursor: None };
        term.resize();
        Ok(term)
    }

    /// Enable/disable mouse reporting (clicks + wheel, SGR encoding).
    pub fn mouse(&mut self, on: bool) {
        let mut out = io::stdout();
        let _ = out.write_all(if on { b"\x1b[?1000h\x1b[?1006h" } else { b"\x1b[?1006l\x1b[?1000l" });
        let _ = out.flush();
    }

    pub fn leave(&mut self) {
        restore_raw();
        let _ = io::stdout().flush();
    }

    fn size() -> (usize, usize) {
        let mut ws = WinSize { rows: 0, cols: 0, xpix: 0, ypix: 0 };
        let ok = unsafe { ioctl(1, TIOCGWINSZ, &mut ws as *mut WinSize) } == 0;
        if ok && ws.cols > 0 && ws.rows > 0 {
            (ws.cols as usize, ws.rows as usize)
        } else {
            (80, 24)
        }
    }

    /// Re-read the window size; returns true if it changed (forces full redraw).
    pub fn resize(&mut self) -> bool {
        let (w, h) = Term::size();
        if w == self.w && h == self.h {
            return false;
        }
        self.w = w;
        self.h = h;
        let blank = Cell { ch: ' ', st: Style::new(Color::Default, Color::Default) };
        self.cells = vec![blank; w * h];
        self.prev = vec![Cell { ch: '\0', st: blank.st }; w * h];
        true
    }

    pub fn clear(&mut self, st: Style) {
        for c in self.cells.iter_mut() {
            *c = Cell { ch: ' ', st };
        }
        self.cursor = None;
    }

    pub fn put(&mut self, x: usize, y: usize, ch: char, st: Style) {
        if x < self.w && y < self.h {
            self.cells[y * self.w + x] = Cell { ch, st };
        }
    }

    /// Write text, clipped to `max` columns. Returns columns used.
    pub fn text(&mut self, x: usize, y: usize, s: &str, st: Style, max: usize) -> usize {
        let mut n = 0;
        for ch in s.chars() {
            if n >= max || x + n >= self.w {
                break;
            }
            let ch = if ch.is_control() { ' ' } else { ch };
            self.put(x + n, y, ch, st);
            n += 1;
        }
        n
    }

    pub fn fill(&mut self, x: usize, y: usize, w: usize, h: usize, st: Style) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.put(xx, yy, ' ', st);
            }
        }
    }

    /// Box with given 6 glyphs: [tl, tr, bl, br, horizontal, vertical]
    pub fn frame(&mut self, x: usize, y: usize, w: usize, h: usize, g: [char; 6], st: Style) {
        if w < 2 || h < 2 {
            return;
        }
        for xx in x + 1..x + w - 1 {
            self.put(xx, y, g[4], st);
            self.put(xx, y + h - 1, g[4], st);
        }
        for yy in y + 1..y + h - 1 {
            self.put(x, yy, g[5], st);
            self.put(x + w - 1, yy, g[5], st);
        }
        self.put(x, y, g[0], st);
        self.put(x + w - 1, y, g[1], st);
        self.put(x, y + h - 1, g[2], st);
        self.put(x + w - 1, y + h - 1, g[3], st);
    }

    fn nearest_slot(&self, c: Color, bg: bool) -> u8 {
        let slots = self.vt_slots.unwrap();
        let (r, g, b) = match c {
            Color::Rgb(r, g, b) => (r as i32, g as i32, b as i32),
            Color::Default => return if bg { 0 } else { 7 },
        };
        let range = if bg { 0..8 } else { 0..16 };
        let mut best = (i32::MAX, 0u8);
        for i in range {
            if let Color::Rgb(sr, sg, sb) = slots[i] {
                let d = (r - sr as i32).pow(2) + (g - sg as i32).pow(2) + (b - sb as i32).pow(2);
                if d < best.0 {
                    best = (d, i as u8);
                }
            }
        }
        best.1
    }

    fn sgr(&self, st: Style, out: &mut Vec<u8>) {
        out.extend_from_slice(b"\x1b[0");
        if st.bold {
            out.extend_from_slice(b";1");
        }
        if st.underline {
            out.extend_from_slice(b";4");
        }
        if self.vt_slots.is_some() {
            let f = self.nearest_slot(st.fg, false);
            let b = self.nearest_slot(st.bg, true);
            let fcode = if f < 8 { 30 + f as u32 } else { 90 + (f - 8) as u32 };
            out.extend_from_slice(format!(";{};{}", fcode, 40 + b as u32).as_bytes());
        } else {
            if let Color::Rgb(r, g, b) = st.fg {
                out.extend_from_slice(format!(";38;2;{};{};{}", r, g, b).as_bytes());
            }
            if let Color::Rgb(r, g, b) = st.bg {
                out.extend_from_slice(format!(";48;2;{};{};{}", r, g, b).as_bytes());
            }
        }
        out.push(b'm');
    }

    /// Flush changed rows to the terminal.
    pub fn present(&mut self) -> io::Result<()> {
        let mut out = Vec::with_capacity(self.w * self.h * 4);
        out.extend_from_slice(b"\x1b[?25l");
        for y in 0..self.h {
            let row = y * self.w..(y + 1) * self.w;
            if self.cells[row.clone()] == self.prev[row.clone()] {
                continue;
            }
            out.extend_from_slice(format!("\x1b[{};1H", y + 1).as_bytes());
            let mut last: Option<Style> = None;
            for x in 0..self.w {
                let c = self.cells[y * self.w + x];
                if last != Some(c.st) {
                    self.sgr(c.st, &mut out);
                    last = Some(c.st);
                }
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.ch.encode_utf8(&mut buf).as_bytes());
            }
        }
        self.prev.clone_from(&self.cells);
        out.extend_from_slice(b"\x1b[0m");
        if let Some((x, y)) = self.cursor {
            out.extend_from_slice(format!("\x1b[{};{}H\x1b[?25h", y + 1, x + 1).as_bytes());
        }
        let mut so = io::stdout();
        so.write_all(&out)?;
        so.flush()
    }

    pub fn force_redraw(&mut self) {
        for c in self.prev.iter_mut() {
            c.ch = '\0';
        }
    }

    /// Read pending keys (waits up to ~100 ms).
    pub fn keys(&mut self) -> Vec<Key> {
        let mut buf = [0u8; 256];
        if let Ok(n) = io::stdin().read(&mut buf) {
            self.pending.extend_from_slice(&buf[..n]);
        }
        let mut keys = vec![];
        let mut i = 0;
        let p = std::mem::take(&mut self.pending);
        while i < p.len() {
            let (k, used) = decode(&p[i..]);
            i += used.max(1);
            if let Some(k) = k {
                keys.push(k);
            }
        }
        keys
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        restore_raw();
    }
}

fn decode(b: &[u8]) -> (Option<Key>, usize) {
    match b[0] {
        0x1b => {
            if b.len() == 1 {
                return (Some(Key::Esc), 1);
            }
            if b[1] == b'O' && b.len() >= 3 {
                let k = match b[2] {
                    b'P' => Key::F(1),
                    b'Q' => Key::F(2),
                    b'R' => Key::F(3),
                    b'S' => Key::F(4),
                    b'H' => Key::Home,
                    b'F' => Key::End,
                    b'A' => Key::Up,
                    b'B' => Key::Down,
                    b'C' => Key::Right,
                    b'D' => Key::Left,
                    _ => return (None, 3),
                };
                return (Some(k), 3);
            }
            if b[1] == b'[' && b.len() >= 3 && b[2] == b'<' {
                // SGR mouse: ESC [ < btn ; col ; row (M | m)
                let mut j = 3;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';') {
                    j += 1;
                }
                if j >= b.len() {
                    return (None, b.len());
                }
                let v: Vec<usize> = std::str::from_utf8(&b[3..j]).unwrap_or("").split(';').filter_map(|x| x.parse().ok()).collect();
                if v.len() == 3 && (b[j] == b'M' || b[j] == b'm') {
                    return (Some(Key::Mouse(v[0] as u8, v[1].saturating_sub(1), v[2].saturating_sub(1), b[j] == b'M')), j + 1);
                }
                return (None, j + 1);
            }
            if b[1] == b'[' {
                // Linux console F1-F5: ESC [ [ A..E
                if b.len() >= 4 && b[2] == b'[' {
                    let n = b[3].wrapping_sub(b'A');
                    return (if n < 5 { Some(Key::F(n + 1)) } else { None }, 4);
                }
                let mut j = 2;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b';') {
                    j += 1;
                }
                if j >= b.len() {
                    return (None, b.len());
                }
                let params = std::str::from_utf8(&b[2..j]).unwrap_or("");
                let first: u32 = params.split(';').next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let k = match b[j] {
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    b'C' => Some(Key::Right),
                    b'D' => Some(Key::Left),
                    b'H' => Some(Key::Home),
                    b'F' => Some(Key::End),
                    b'Z' => Some(Key::BackTab),
                    b'~' => match first {
                        1 | 7 => Some(Key::Home),
                        4 | 8 => Some(Key::End),
                        3 => Some(Key::Delete),
                        5 => Some(Key::PageUp),
                        6 => Some(Key::PageDown),
                        11..=15 => Some(Key::F((first - 10) as u8)),
                        17..=21 => Some(Key::F((first - 11) as u8)),
                        23 | 24 => Some(Key::F((first - 12) as u8)),
                        _ => None,
                    },
                    _ => None,
                };
                return (k, j + 1);
            }
            // Alt+key: treat as Esc then key
            (Some(Key::Esc), 1)
        }
        0x0d | 0x0a => (Some(Key::Enter), 1),
        0x09 => (Some(Key::Tab), 1),
        0x7f | 0x08 => (Some(Key::Backspace), 1),
        c @ 1..=26 => (Some(Key::Ctrl((b'a' + c - 1) as char)), 1),
        c if c < 0x80 => (Some(Key::Char(c as char)), 1),
        c => {
            let len = if c >= 0xf0 { 4 } else if c >= 0xe0 { 3 } else { 2 };
            if b.len() < len {
                return (None, b.len());
            }
            match std::str::from_utf8(&b[..len]) {
                Ok(s) => (s.chars().next().map(Key::Char), len),
                Err(_) => (None, 1),
            }
        }
    }
}

/// Single-line text input.
#[derive(Default, Clone)]
pub struct Input {
    pub buf: String,
    pub pos: usize, // in chars
}

impl Input {
    pub fn with(s: &str) -> Input {
        Input { buf: s.to_string(), pos: s.chars().count() }
    }
    pub fn set(&mut self, s: &str) {
        self.buf = s.to_string();
        self.pos = s.chars().count();
    }
    fn byte_at(&self, pos: usize) -> usize {
        self.buf.char_indices().nth(pos).map(|(i, _)| i).unwrap_or(self.buf.len())
    }
    /// Returns true if the key was consumed.
    pub fn handle(&mut self, k: Key) -> bool {
        match k {
            Key::Char(c) => {
                let i = self.byte_at(self.pos);
                self.buf.insert(i, c);
                self.pos += 1;
            }
            Key::Backspace if self.pos > 0 => {
                self.pos -= 1;
                let i = self.byte_at(self.pos);
                self.buf.remove(i);
            }
            Key::Delete if self.pos < self.buf.chars().count() => {
                let i = self.byte_at(self.pos);
                self.buf.remove(i);
            }
            Key::Left if self.pos > 0 => self.pos -= 1,
            Key::Right if self.pos < self.buf.chars().count() => self.pos += 1,
            Key::Home | Key::Ctrl('a') => self.pos = 0,
            Key::End | Key::Ctrl('e') => self.pos = self.buf.chars().count(),
            Key::Ctrl('u') => {
                self.buf.clear();
                self.pos = 0;
            }
            Key::Ctrl('w') => {
                let chars: Vec<char> = self.buf.chars().collect();
                let mut p = self.pos;
                while p > 0 && chars[p - 1] == ' ' {
                    p -= 1;
                }
                while p > 0 && chars[p - 1] != ' ' {
                    p -= 1;
                }
                let s: String = chars[..p].iter().chain(chars[self.pos..].iter()).collect();
                self.buf = s;
                self.pos = p;
            }
            Key::Backspace | Key::Delete | Key::Left | Key::Right => {}
            _ => return false,
        }
        true
    }
    /// Draw at (x,y) within `w` columns; sets the terminal cursor if `focus`.
    pub fn draw(&self, t: &mut Term, x: usize, y: usize, w: usize, st: Style, focus: bool, mask: Option<char>) {
        let chars: Vec<char> = match mask {
            Some(m) => std::iter::repeat(m).take(self.buf.chars().count()).collect(),
            None => self.buf.chars().collect(),
        };
        let start = if self.pos >= w { self.pos + 1 - w } else { 0 };
        t.fill(x, y, w, 1, st);
        for (i, ch) in chars.iter().skip(start).take(w).enumerate() {
            t.put(x + i, y, *ch, st);
        }
        if focus {
            t.cursor = Some((x + self.pos - start, y));
        }
    }
}
