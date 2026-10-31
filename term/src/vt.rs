//! Terminal emulation core: grid, scrollback, cursor, modes, SGR, OSC.
//! Pure logic (no Wayland), unit-tested at the bottom of this file.
use std::collections::VecDeque;
use std::time::Instant;
use unicode_width::UnicodeWidthChar;

pub const BOLD: u16 = 1;
pub const DIM: u16 = 2;
pub const ITALIC: u16 = 4;
pub const UNDERLINE: u16 = 8;
pub const REVERSE: u16 = 16;
pub const HIDDEN: u16 = 32;
pub const STRIKE: u16 = 64;
pub const WIDE: u16 = 128; // first half of a double-width char
pub const SPACER: u16 = 256; // second half (draw nothing)

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Color {
    Default,
    Idx(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: u16,
}

impl Cell {
    pub const BLANK: Cell = Cell { ch: ' ', fg: Color::Default, bg: Color::Default, flags: 0 };
    fn blank_with(bg: Color) -> Cell {
        Cell { ch: ' ', fg: Color::Default, bg, flags: 0 }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CursorShape {
    Block,
    Underline,
    Beam,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MouseMode {
    Off,
    Click,  // 1000
    Drag,   // 1002
    Motion, // 1003
}

/// An OSC 133 command block (shell integration), in absolute line numbers.
#[derive(Clone, Debug)]
pub struct Block {
    pub prompt_line: u64,
    pub cmd_line: Option<u64>,
    pub end_line: Option<u64>,
    pub exit: Option<i32>,
    pub started: Option<Instant>,
    pub duration: Option<f32>,
}

#[derive(Clone, Copy)]
struct Saved {
    cx: usize,
    cy: usize,
    attr: Cell,
    origin: bool,
    g0_graphics: bool,
}

pub enum Event {
    Title(String),
    Clipboard(String),
    Bell,
}

pub struct Term {
    pub cols: usize,
    pub rows: usize,
    main: Vec<Vec<Cell>>,
    alt: Vec<Vec<Cell>>,
    pub alt_active: bool,
    pub scrollback: VecDeque<Vec<Cell>>,
    pub max_scrollback: usize,
    /// lines that have scrolled off the top of the main screen (absolute line base)
    pub scrolled: u64,
    pub cx: usize,
    pub cy: usize,
    wrap_pending: bool,
    attr: Cell,
    saved_main: Option<Saved>,
    saved_alt: Option<Saved>,
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub cursor_visible: bool,
    pub cursor_shape: CursorShape,
    autowrap: bool,
    origin: bool,
    insert: bool,
    lnm: bool,
    pub mouse: MouseMode,
    pub mouse_sgr: bool,
    pub focus_events: bool,
    g0_graphics: bool,
    last_char: char,
    pub title: String,
    pub cwd: Option<String>,
    pub blocks: Vec<Block>,
    pub responses: Vec<u8>,
    pub events: Vec<Event>,
    pub dirty: bool,
    /// default colors for OSC 10/11 queries
    pub default_fg: (u8, u8, u8),
    pub default_bg: (u8, u8, u8),
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Term {
        let cols = cols.max(2);
        let rows = rows.max(2);
        let mut t = Term {
            cols,
            rows,
            main: vec![vec![Cell::BLANK; cols]; rows],
            alt: vec![vec![Cell::BLANK; cols]; rows],
            alt_active: false,
            scrollback: VecDeque::new(),
            max_scrollback: 10_000,
            scrolled: 0,
            cx: 0,
            cy: 0,
            wrap_pending: false,
            attr: Cell::BLANK,
            saved_main: None,
            saved_alt: None,
            top: 0,
            bottom: rows - 1,
            tabs: vec![],
            app_cursor: false,
            app_keypad: false,
            bracketed_paste: false,
            cursor_visible: true,
            cursor_shape: CursorShape::Block,
            autowrap: true,
            origin: false,
            insert: false,
            lnm: false,
            mouse: MouseMode::Off,
            mouse_sgr: false,
            focus_events: false,
            g0_graphics: false,
            last_char: ' ',
            title: String::new(),
            cwd: None,
            blocks: vec![],
            responses: vec![],
            events: vec![],
            dirty: true,
            default_fg: (220, 220, 220),
            default_bg: (0, 0, 0),
        };
        t.reset_tabs();
        t
    }

    fn reset_tabs(&mut self) {
        self.tabs = (0..self.cols).map(|i| i % 8 == 0 && i != 0).collect();
    }

    pub fn screen(&self) -> &Vec<Vec<Cell>> {
        if self.alt_active { &self.alt } else { &self.main }
    }
    fn screen_mut(&mut self) -> &mut Vec<Vec<Cell>> {
        if self.alt_active { &mut self.alt } else { &mut self.main }
    }

    /// Absolute line number of screen row `y` (main screen numbering).
    pub fn abs_line(&self, y: usize) -> u64 {
        self.scrolled + y as u64
    }

    /// Line at absolute number (scrollback or screen), for selection/gutter.
    pub fn line_at(&self, abs: u64) -> Option<&Vec<Cell>> {
        let first = self.scrolled.saturating_sub(self.scrollback.len() as u64);
        if abs < first {
            return None;
        }
        if abs < self.scrolled {
            return self.scrollback.get((abs - first) as usize);
        }
        self.screen().get((abs - self.scrolled) as usize)
    }

    // ------------------------------------------------------------ resize
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(2);
        let rows = rows.max(2);
        if cols == self.cols && rows == self.rows {
            return;
        }
        for scr in [&mut self.main, &mut self.alt] {
            for line in scr.iter_mut() {
                line.resize(cols, Cell::BLANK);
                if let Some(last) = line.last_mut() {
                    if last.flags & WIDE != 0 {
                        *last = Cell::BLANK;
                    }
                }
            }
        }
        // rows: main screen keeps the cursor line visible, spilling into scrollback
        if rows < self.rows {
            let mut remove = self.rows - rows;
            // drop blank lines below the cursor first
            while remove > 0 && self.main.len() > self.cy + 1 && self.main.last().map(|l| l.iter().all(|c| *c == Cell::BLANK)).unwrap_or(false) {
                self.main.pop();
                remove -= 1;
            }
            for _ in 0..remove {
                let l = self.main.remove(0);
                self.push_scrollback(l);
                self.cy = self.cy.saturating_sub(1);
            }
            self.alt.truncate(rows);
        } else if rows > self.rows {
            let mut add = rows - self.rows;
            while add > 0 && !self.alt_active {
                match self.scrollback.pop_back() {
                    Some(mut l) => {
                        l.resize(cols, Cell::BLANK);
                        self.main.insert(0, l);
                        self.scrolled -= 1;
                        self.cy += 1;
                        add -= 1;
                    }
                    None => break,
                }
            }
            while self.main.len() < rows {
                self.main.push(vec![Cell::BLANK; cols]);
            }
            while self.alt.len() < rows {
                self.alt.push(vec![Cell::BLANK; cols]);
            }
        }
        self.main.truncate(rows);
        self.alt.truncate(rows);
        while self.main.len() < rows {
            self.main.push(vec![Cell::BLANK; cols]);
        }
        while self.alt.len() < rows {
            self.alt.push(vec![Cell::BLANK; cols]);
        }
        for scr in [&mut self.main, &mut self.alt] {
            for line in scr.iter_mut() {
                line.resize(cols, Cell::BLANK);
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        self.cx = self.cx.min(cols - 1);
        self.cy = self.cy.min(rows - 1);
        self.wrap_pending = false;
        self.reset_tabs();
        self.dirty = true;
    }

    fn push_scrollback(&mut self, line: Vec<Cell>) {
        self.scrollback.push_back(line);
        self.scrolled += 1;
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }
    }

    // ------------------------------------------------------------ primitives
    fn blank(&self) -> Cell {
        Cell::blank_with(self.attr.bg)
    }

    fn scroll_up(&mut self, n: usize) {
        let keep = self.top == 0 && !self.alt_active;
        self.scroll_up_inner(n, keep);
    }

    fn scroll_up_inner(&mut self, n: usize, keep: bool) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        let blank = self.blank();
        let cols = self.cols;
        for _ in 0..n {
            let line = self.screen_mut().remove(top);
            self.screen_mut().insert(bottom, vec![blank; cols]);
            if keep {
                self.push_scrollback(line);
            }
        }
        self.dirty = true;
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let n = n.min(bottom - top + 1);
        let blank = self.blank();
        let cols = self.cols;
        for _ in 0..n {
            self.screen_mut().remove(bottom);
            self.screen_mut().insert(top, vec![blank; cols]);
        }
        self.dirty = true;
    }

    fn linefeed(&mut self) {
        if self.cy == self.bottom {
            self.scroll_up(1);
        } else if self.cy < self.rows - 1 {
            self.cy += 1;
        }
    }

    fn put_char(&mut self, c: char) {
        let c = if self.g0_graphics { dec_graphics(c) } else { c };
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if w == 0 {
            return; // combining marks are dropped (single-codepoint cells)
        }
        if self.wrap_pending {
            if self.autowrap {
                self.cx = 0;
                self.linefeed();
            }
            self.wrap_pending = false;
        }
        if w == 2 && self.cx == self.cols - 1 {
            if self.autowrap {
                let (cx, cy) = (self.cx, self.cy);
                let b = self.blank();
                self.screen_mut()[cy][cx] = b;
                self.cx = 0;
                self.linefeed();
            } else {
                return;
            }
        }
        let (cx, cy, cols) = (self.cx, self.cy, self.cols);
        if self.insert {
            let line = &mut self.screen_mut()[cy];
            for _ in 0..w {
                line.insert(cx, Cell::BLANK);
                line.truncate(cols);
            }
        }
        let mut cell = self.attr;
        cell.ch = c;
        cell.flags &= !(WIDE | SPACER);
        {
            let line = &mut self.screen_mut()[cy];
            // breaking a wide char in half leaves no orphans
            if line[cx].flags & SPACER != 0 && cx > 0 {
                line[cx - 1] = Cell::BLANK;
            }
            if line[cx].flags & WIDE != 0 && cx + 1 < cols {
                line[cx + 1] = Cell::BLANK;
            }
            if w == 2 {
                cell.flags |= WIDE;
                line[cx] = cell;
                let mut sp = cell;
                sp.ch = ' ';
                sp.flags = (sp.flags & !WIDE) | SPACER;
                line[cx + 1] = sp;
            } else {
                line[cx] = cell;
            }
        }
        self.last_char = c;
        self.cx += w;
        if self.cx >= self.cols {
            self.cx = self.cols - 1;
            self.wrap_pending = true;
        }
        self.dirty = true;
    }

    fn erase_cells(&mut self, y: usize, from: usize, to: usize) {
        let b = self.blank();
        let to = to.min(self.cols);
        let line = &mut self.screen_mut()[y];
        for c in line.iter_mut().take(to).skip(from) {
            *c = b;
        }
        self.dirty = true;
    }

    fn goto(&mut self, x: usize, y: usize) {
        let (ymin, ymax) = if self.origin { (self.top, self.bottom) } else { (0, self.rows - 1) };
        self.cx = x.min(self.cols - 1);
        self.cy = (y + if self.origin { self.top } else { 0 }).clamp(ymin, ymax);
        self.wrap_pending = false;
    }

    fn save_cursor(&mut self) {
        let s = Saved { cx: self.cx, cy: self.cy, attr: self.attr, origin: self.origin, g0_graphics: self.g0_graphics };
        if self.alt_active { self.saved_alt = Some(s) } else { self.saved_main = Some(s) }
    }

    fn restore_cursor(&mut self) {
        let s = if self.alt_active { self.saved_alt } else { self.saved_main };
        if let Some(s) = s {
            self.cx = s.cx.min(self.cols - 1);
            self.cy = s.cy.min(self.rows - 1);
            self.attr = s.attr;
            self.origin = s.origin;
            self.g0_graphics = s.g0_graphics;
        } else {
            self.cx = 0;
            self.cy = 0;
        }
        self.wrap_pending = false;
    }

    fn set_alt(&mut self, on: bool, save: bool, clear: bool) {
        if on == self.alt_active {
            return;
        }
        if on {
            if save {
                self.save_cursor();
            }
            self.alt_active = true;
            if clear {
                let b = Cell::BLANK;
                for l in self.alt.iter_mut() {
                    for c in l.iter_mut() {
                        *c = b;
                    }
                }
            }
        } else {
            self.alt_active = false;
            if save {
                self.restore_cursor();
            }
        }
        self.top = 0;
        self.bottom = self.rows - 1;
        self.dirty = true;
    }

    pub fn full_reset(&mut self) {
        let (c, r, sb, fg, bg) = (self.cols, self.rows, std::mem::take(&mut self.scrollback), self.default_fg, self.default_bg);
        let scrolled = self.scrolled;
        *self = Term::new(c, r);
        self.scrollback = sb;
        self.scrolled = scrolled;
        self.default_fg = fg;
        self.default_bg = bg;
    }

    // ------------------------------------------------------------ SGR
    fn sgr(&mut self, params: &vte::Params) {
        let groups: Vec<Vec<u16>> = params.iter().map(|p| p.to_vec()).collect();
        if groups.is_empty() {
            self.attr = Cell { ch: ' ', ..Cell::BLANK };
            return;
        }
        let mut i = 0;
        while i < groups.len() {
            let g = &groups[i];
            let p = g[0];
            match p {
                0 => {
                    self.attr.fg = Color::Default;
                    self.attr.bg = Color::Default;
                    self.attr.flags = 0;
                }
                1 => self.attr.flags |= BOLD,
                2 => self.attr.flags |= DIM,
                3 => self.attr.flags |= ITALIC,
                4 => {
                    if g.len() > 1 && g[1] == 0 {
                        self.attr.flags &= !UNDERLINE
                    } else {
                        self.attr.flags |= UNDERLINE
                    }
                }
                7 => self.attr.flags |= REVERSE,
                8 => self.attr.flags |= HIDDEN,
                9 => self.attr.flags |= STRIKE,
                21 => self.attr.flags |= UNDERLINE,
                22 => self.attr.flags &= !(BOLD | DIM),
                23 => self.attr.flags &= !ITALIC,
                24 => self.attr.flags &= !UNDERLINE,
                27 => self.attr.flags &= !REVERSE,
                28 => self.attr.flags &= !HIDDEN,
                29 => self.attr.flags &= !STRIKE,
                30..=37 => self.attr.fg = Color::Idx((p - 30) as u8),
                39 => self.attr.fg = Color::Default,
                40..=47 => self.attr.bg = Color::Idx((p - 40) as u8),
                49 => self.attr.bg = Color::Default,
                90..=97 => self.attr.fg = Color::Idx((p - 90 + 8) as u8),
                100..=107 => self.attr.bg = Color::Idx((p - 100 + 8) as u8),
                38 | 48 | 58 => {
                    let (color, used) = if g.len() > 1 {
                        (parse_ext_color(&g[1..]), 0)
                    } else {
                        let rest: Vec<u16> = groups[i + 1..].iter().map(|x| x[0]).collect();
                        let (c, n) = parse_ext_color_sep(&rest);
                        (c, n)
                    };
                    i += used;
                    if let Some(c) = color {
                        match p {
                            38 => self.attr.fg = c,
                            48 => self.attr.bg = c,
                            _ => {} // underline colour: ignored
                        }
                    }
                }
                59 => {}
                _ => {}
            }
            i += 1;
        }
    }

    // ------------------------------------------------------------ modes
    fn set_mode(&mut self, private: bool, params: &vte::Params, on: bool) {
        for p in params.iter() {
            let m = p[0];
            if private {
                match m {
                    1 => self.app_cursor = on,
                    6 => {
                        self.origin = on;
                        self.goto(0, 0);
                    }
                    7 => self.autowrap = on,
                    25 => self.cursor_visible = on,
                    47 | 1047 => self.set_alt(on, false, on),
                    1048 => {
                        if on { self.save_cursor() } else { self.restore_cursor() }
                    }
                    1049 => self.set_alt(on, true, on),
                    1000 => self.mouse = if on { MouseMode::Click } else { MouseMode::Off },
                    1002 => self.mouse = if on { MouseMode::Drag } else { MouseMode::Off },
                    1003 => self.mouse = if on { MouseMode::Motion } else { MouseMode::Off },
                    1004 => self.focus_events = on,
                    1006 => self.mouse_sgr = on,
                    2004 => self.bracketed_paste = on,
                    _ => {}
                }
            } else {
                match m {
                    4 => self.insert = on,
                    20 => self.lnm = on,
                    _ => {}
                }
            }
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------ OSC 133 blocks
    fn osc133(&mut self, kind: &str, arg: Option<&str>) {
        let line = self.abs_line(self.cy);
        match kind {
            "A" => {
                self.blocks.push(Block { prompt_line: line, cmd_line: None, end_line: None, exit: None, started: None, duration: None });
                if self.blocks.len() > 2000 {
                    self.blocks.drain(0..500);
                }
            }
            "B" => {}
            "C" => {
                if let Some(b) = self.blocks.last_mut() {
                    if b.cmd_line.is_none() {
                        b.cmd_line = Some(line);
                        b.started = Some(Instant::now());
                    }
                }
            }
            "D" => {
                if let Some(b) = self.blocks.last_mut() {
                    if b.cmd_line.is_some() && b.end_line.is_none() {
                        b.end_line = Some(line);
                        b.exit = arg.and_then(|a| a.split(';').next()).and_then(|a| a.parse().ok()).or(Some(0));
                        b.duration = b.started.map(|s| s.elapsed().as_secs_f32());
                    }
                }
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Last finished command block (for the HUD).
    pub fn last_finished(&self) -> Option<&Block> {
        self.blocks.iter().rev().find(|b| b.end_line.is_some())
    }

    // ------------------------------------------------------------ selection text
    pub fn text_between(&self, a: (u64, usize), b: (u64, usize)) -> String {
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        let mut out = String::new();
        for line_no in s.0..=e.0 {
            let Some(line) = self.line_at(line_no) else { continue };
            let from = if line_no == s.0 { s.1 } else { 0 };
            let to = if line_no == e.0 { (e.1 + 1).min(line.len()) } else { line.len() };
            let mut l: String = line[from.min(line.len())..to].iter().filter(|c| c.flags & SPACER == 0).map(|c| c.ch).collect();
            if line_no != e.0 {
                l = l.trim_end().to_string();
                l.push('\n');
            }
            out.push_str(&l);
        }
        out
    }
}

fn parse_ext_color(sub: &[u16]) -> Option<Color> {
    // colon form: 38:5:n  or 38:2:[cs]:r:g:b
    match sub.first() {
        Some(5) => sub.get(1).map(|n| Color::Idx(*n as u8)),
        Some(2) => {
            let v: Vec<u16> = sub[1..].to_vec();
            let rgb = if v.len() >= 4 { &v[v.len() - 3..] } else if v.len() == 3 { &v[..] } else { return None };
            Some(Color::Rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8))
        }
        _ => None,
    }
}

/// semicolon form: 38;5;n or 38;2;r;g;b — returns colour and params consumed
fn parse_ext_color_sep(rest: &[u16]) -> (Option<Color>, usize) {
    match rest.first() {
        Some(5) if rest.len() >= 2 => (Some(Color::Idx(rest[1] as u8)), 2),
        Some(2) if rest.len() >= 4 => (Some(Color::Rgb(rest[1] as u8, rest[2] as u8, rest[3] as u8)), 4),
        Some(_) => (None, 1),
        None => (None, 0),
    }
}

fn dec_graphics(c: char) -> char {
    match c {
        '`' => '◆', 'a' => '▒', 'f' => '°', 'g' => '±', 'j' => '┘', 'k' => '┐', 'l' => '┌', 'm' => '└',
        'n' => '┼', 'o' => '⎺', 'p' => '⎻', 'q' => '─', 'r' => '⎼', 's' => '⎽', 't' => '├', 'u' => '┤',
        'v' => '┴', 'w' => '┬', 'x' => '│', 'y' => '≤', 'z' => '≥', '{' => 'π', '|' => '≠', '}' => '£', '~' => '·',
        _ => c,
    }
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = vec![];
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' => continue,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

impl vte::Perform for Term {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.events.push(Event::Bell),
            0x08 => {
                self.cx = self.cx.saturating_sub(1);
                self.wrap_pending = false;
            }
            0x09 => {
                let mut x = self.cx + 1;
                while x < self.cols - 1 && !self.tabs[x] {
                    x += 1;
                }
                self.cx = x.min(self.cols - 1);
            }
            0x0a..=0x0c => {
                self.linefeed();
                if self.lnm {
                    self.cx = 0;
                }
                self.wrap_pending = false;
            }
            0x0d => {
                self.cx = 0;
                self.wrap_pending = false;
            }
            0x0e => self.g0_graphics = true,  // SO (approximation: G1 = graphics)
            0x0f => self.g0_graphics = false, // SI
            _ => {}
        }
        self.dirty = true;
    }

    fn csi_dispatch(&mut self, params: &vte::Params, inter: &[u8], _ignore: bool, action: char) {
        let ps: Vec<u16> = params.iter().map(|p| p[0]).collect();
        let p0 = |d: u16| -> usize { ps.first().copied().filter(|v| *v != 0).unwrap_or(d) as usize };
        let p1 = |d: u16| -> usize { ps.get(1).copied().filter(|v| *v != 0).unwrap_or(d) as usize };
        let private = inter.first() == Some(&b'?');
        match (action, inter) {
            ('m', []) => self.sgr(params),
            ('h', _) => self.set_mode(private, params, true),
            ('l', _) => self.set_mode(private, params, false),
            ('A', []) => {
                let top = if self.cy >= self.top { self.top } else { 0 };
                self.cy = self.cy.saturating_sub(p0(1)).max(top);
                self.wrap_pending = false;
            }
            ('B', []) | ('e', []) => {
                let bot = if self.cy <= self.bottom { self.bottom } else { self.rows - 1 };
                self.cy = (self.cy + p0(1)).min(bot);
                self.wrap_pending = false;
            }
            ('C', []) | ('a', []) => {
                self.cx = (self.cx + p0(1)).min(self.cols - 1);
                self.wrap_pending = false;
            }
            ('D', []) => {
                self.cx = self.cx.saturating_sub(p0(1));
                self.wrap_pending = false;
            }
            ('E', []) => {
                self.cy = (self.cy + p0(1)).min(self.bottom);
                self.cx = 0;
            }
            ('F', []) => {
                self.cy = self.cy.saturating_sub(p0(1)).max(self.top);
                self.cx = 0;
            }
            ('G', []) | ('`', []) => {
                self.cx = (p0(1) - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            ('d', []) => {
                let x = self.cx;
                self.goto(x, p0(1) - 1);
            }
            ('H', []) | ('f', []) => self.goto(p1(1) - 1, p0(1) - 1),
            ('I', []) => {
                for _ in 0..p0(1) {
                    self.execute(0x09);
                }
            }
            ('Z', []) => {
                for _ in 0..p0(1) {
                    let mut x = self.cx.saturating_sub(1);
                    while x > 0 && !self.tabs[x] {
                        x -= 1;
                    }
                    self.cx = x;
                }
            }
            ('J', []) => {
                let (cx, cy, rows, cols) = (self.cx, self.cy, self.rows, self.cols);
                match ps.first().copied().unwrap_or(0) {
                    0 => {
                        self.erase_cells(cy, cx, cols);
                        for y in cy + 1..rows {
                            self.erase_cells(y, 0, cols);
                        }
                    }
                    1 => {
                        for y in 0..cy {
                            self.erase_cells(y, 0, cols);
                        }
                        self.erase_cells(cy, 0, cx + 1);
                    }
                    2 => {
                        for y in 0..rows {
                            self.erase_cells(y, 0, cols);
                        }
                    }
                    3 => {
                        self.scrollback.clear();
                        self.blocks.retain(|b| b.prompt_line >= self.scrolled);
                    }
                    _ => {}
                }
            }
            ('K', []) => {
                let (cx, cy, cols) = (self.cx, self.cy, self.cols);
                match ps.first().copied().unwrap_or(0) {
                    0 => self.erase_cells(cy, cx, cols),
                    1 => self.erase_cells(cy, 0, cx + 1),
                    2 => self.erase_cells(cy, 0, cols),
                    _ => {}
                }
            }
            ('X', []) => {
                let (cx, cy) = (self.cx, self.cy);
                self.erase_cells(cy, cx, cx + p0(1));
            }
            ('@', []) => {
                let (cx, cy, cols, b) = (self.cx, self.cy, self.cols, self.blank());
                let line = &mut self.screen_mut()[cy];
                for _ in 0..p0(1).min(cols - cx) {
                    line.insert(cx, b);
                    line.truncate(cols);
                }
                self.dirty = true;
            }
            ('P', []) => {
                let (cx, cy, cols, b) = (self.cx, self.cy, self.cols, self.blank());
                let line = &mut self.screen_mut()[cy];
                for _ in 0..p0(1).min(cols - cx) {
                    line.remove(cx);
                    line.push(b);
                }
                self.dirty = true;
            }
            ('L', []) => {
                if self.cy >= self.top && self.cy <= self.bottom {
                    let (old_top, n) = (self.top, p0(1));
                    self.top = self.cy;
                    self.scroll_down(n);
                    self.top = old_top;
                    self.cx = 0;
                }
            }
            ('M', []) => {
                if self.cy >= self.top && self.cy <= self.bottom {
                    let (old_top, n) = (self.top, p0(1));
                    self.top = self.cy;
                    // deleting lines inside the region never feeds scrollback
                    self.scroll_up_inner(n, false);
                    self.top = old_top;
                    self.cx = 0;
                }
            }
            ('S', []) => self.scroll_up(p0(1)),
            ('T', []) => self.scroll_down(p0(1)),
            ('b', []) => {
                let c = self.last_char;
                for _ in 0..p0(1).min(65535) {
                    self.put_char(c);
                }
            }
            ('c', []) => self.responses.extend_from_slice(b"\x1b[?62;22c"),
            ('c', [b'>']) => self.responses.extend_from_slice(b"\x1b[>1;10;0c"),
            ('n', []) => match ps.first().copied().unwrap_or(0) {
                5 => self.responses.extend_from_slice(b"\x1b[0n"),
                6 => {
                    let y = if self.origin { self.cy - self.top } else { self.cy };
                    self.responses.extend_from_slice(format!("\x1b[{};{}R", y + 1, self.cx + 1).as_bytes());
                }
                _ => {}
            },
            ('r', []) => {
                let top = p0(1) - 1;
                let bottom = p1(self.rows as u16) - 1;
                if top < bottom && bottom < self.rows {
                    self.top = top;
                    self.bottom = bottom;
                    self.goto(0, 0);
                }
            }
            ('s', []) => self.save_cursor(),
            ('u', []) => self.restore_cursor(),
            ('g', []) => match ps.first().copied().unwrap_or(0) {
                0 => {
                    let x = self.cx;
                    if x < self.tabs.len() {
                        self.tabs[x] = false
                    }
                }
                3 => self.tabs.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            ('q', [b' ']) => {
                self.cursor_shape = match ps.first().copied().unwrap_or(0) {
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Beam,
                    _ => CursorShape::Block,
                }
            }
            ('p', [b'!']) => {
                // DECSTR soft reset
                self.attr = Cell::BLANK;
                self.top = 0;
                self.bottom = self.rows - 1;
                self.origin = false;
                self.autowrap = true;
                self.insert = false;
                self.cursor_visible = true;
                self.app_cursor = false;
            }
            ('p', [b'?', b'$']) => {
                // DECRQM: report mode as not recognised/reset (0) — keeps apps from waiting
                let m = ps.first().copied().unwrap_or(0);
                let v = match m {
                    1 => self.app_cursor as u8 + 1,
                    25 => (!self.cursor_visible) as u8 + 1,
                    1049 => (!self.alt_active) as u8 + 1,
                    2004 => (!self.bracketed_paste) as u8 + 1,
                    2026 => 2,
                    _ => 0,
                };
                self.responses.extend_from_slice(format!("\x1b[?{m};{v}$y").as_bytes());
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn esc_dispatch(&mut self, inter: &[u8], _ignore: bool, byte: u8) {
        match (inter, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([b'#'], b'8') => {
                for y in 0..self.rows {
                    for x in 0..self.cols {
                        self.screen_mut()[y][x] = Cell { ch: 'E', ..Cell::BLANK };
                    }
                }
            }
            ([], b'D') => self.linefeed(),
            ([], b'E') => {
                self.linefeed();
                self.cx = 0;
            }
            ([], b'H') => {
                let x = self.cx;
                if x < self.tabs.len() {
                    self.tabs[x] = true
                }
            }
            ([], b'M') => {
                if self.cy == self.top {
                    self.scroll_down(1);
                } else {
                    self.cy = self.cy.saturating_sub(1);
                }
            }
            ([], b'c') => self.full_reset(),
            ([], b'=') => self.app_keypad = true,
            ([], b'>') => self.app_keypad = false,
            ([b'('], b'0') => self.g0_graphics = true,
            ([b'('], _) => self.g0_graphics = false,
            _ => {}
        }
        self.dirty = true;
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        let s = |i: usize| params.get(i).map(|p| String::from_utf8_lossy(p).to_string());
        let term = if bell_terminated { "\x07" } else { "\x1b\\" };
        match s(0).as_deref() {
            Some("0") | Some("2") => {
                if let Some(t) = s(1) {
                    self.title = t.clone();
                    self.events.push(Event::Title(t));
                }
            }
            Some("7") => {
                if let Some(u) = s(1) {
                    let path = u.strip_prefix("file://").map(|r| r.find('/').map(|i| &r[i..]).unwrap_or("/")).unwrap_or(&u);
                    self.cwd = Some(percent_decode(path));
                }
            }
            Some("133") => {
                if let Some(k) = s(1) {
                    let arg = s(2);
                    self.osc133(&k, arg.as_deref());
                }
            }
            Some("52") => {
                if let Some(data) = s(2) {
                    if data != "?" {
                        if let Some(b) = base64_decode(&data) {
                            self.events.push(Event::Clipboard(String::from_utf8_lossy(&b).to_string()));
                        }
                    }
                }
            }
            Some(n @ ("10" | "11")) if s(1).as_deref() == Some("?") => {
                let (r, g, b) = if n == "10" { self.default_fg } else { self.default_bg };
                self.responses.extend_from_slice(
                    format!("\x1b]{n};rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}{term}", r, r, g, g, b, b).as_bytes(),
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut Term, s: &str) {
        let mut p = vte::Parser::new();
        p.advance(t, s.as_bytes());
    }
    fn row(t: &Term, y: usize) -> String {
        t.screen()[y].iter().filter(|c| c.flags & SPACER == 0).map(|c| c.ch).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn text_and_wrap() {
        let mut t = Term::new(5, 3);
        feed(&mut t, "hello world");
        assert_eq!(row(&t, 0), "hello");
        assert_eq!(row(&t, 1), " worl");
        assert_eq!(row(&t, 2), "d");
    }

    #[test]
    fn scrollback_and_cursor() {
        let mut t = Term::new(10, 3);
        feed(&mut t, "a\r\nb\r\nc\r\nd");
        assert_eq!(t.scrollback.len(), 1);
        assert_eq!(row(&t, 0), "b");
        assert_eq!(row(&t, 2), "d");
        feed(&mut t, "\x1b[2;3Hx");
        assert_eq!(row(&t, 1), "c x");
        feed(&mut t, "\x1b[6n");
        assert_eq!(t.responses, b"\x1b[2;4R");
    }

    #[test]
    fn sgr_colors() {
        let mut t = Term::new(10, 2);
        feed(&mut t, "\x1b[1;38;2;10;20;30;48;5;200mX\x1b[0mY\x1b[38:2::1:2:3mZ");
        let c = t.screen()[0][0];
        assert_eq!(c.fg, Color::Rgb(10, 20, 30));
        assert_eq!(c.bg, Color::Idx(200));
        assert!(c.flags & BOLD != 0);
        assert_eq!(t.screen()[0][1].fg, Color::Default);
        assert_eq!(t.screen()[0][2].fg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn alt_screen_and_erase() {
        let mut t = Term::new(6, 3);
        feed(&mut t, "main");
        feed(&mut t, "\x1b[?1049h\x1b[2J\x1b[Halt");
        assert_eq!(row(&t, 0), "alt");
        feed(&mut t, "\x1b[?1049l");
        assert_eq!(row(&t, 0), "main");
        assert_eq!(t.cx, 4);
    }

    #[test]
    fn scroll_region_and_insert_delete() {
        let mut t = Term::new(4, 4);
        feed(&mut t, "1\r\n2\r\n3\r\n4");
        feed(&mut t, "\x1b[2;3r\x1b[2;1H\x1b[M");
        assert_eq!((row(&t, 0), row(&t, 1), row(&t, 2), row(&t, 3)), ("1".into(), "3".into(), "".into(), "4".into()));
        assert_eq!(t.scrollback.len(), 0);
    }

    #[test]
    fn wide_chars_and_dec_graphics() {
        let mut t = Term::new(6, 2);
        feed(&mut t, "日本\x1b(0lqk\x1b(B");
        assert_eq!(row(&t, 0), "日本┌─");
        assert_eq!(row(&t, 1), "┐");
    }

    #[test]
    fn osc_blocks_and_cwd() {
        let mut t = Term::new(20, 5);
        feed(&mut t, "\x1b]7;file://host/home/u/My%20Dir\x07");
        assert_eq!(t.cwd.as_deref(), Some("/home/u/My Dir"));
        feed(&mut t, "\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07\x1b]133;D;1\x07");
        let b = t.last_finished().unwrap();
        assert_eq!(b.exit, Some(1));
        assert_eq!(b.cmd_line, Some(1));
    }

    #[test]
    fn resize_keeps_cursor_line() {
        let mut t = Term::new(10, 5);
        feed(&mut t, "a\r\nb\r\nc\r\nd\r\ne");
        t.resize(10, 3);
        assert_eq!(row(&t, 2), "e");
        assert_eq!(t.cy, 2);
        t.resize(10, 5);
        assert_eq!(row(&t, 0), "a");
        assert_eq!(t.cy, 4);
    }
}
