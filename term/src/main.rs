//! shadow-term — the Shadow TUI Restricted terminal.
//!
//!   shadow-term [--app-id=ID] [--title=T] [--window-size-chars=COLSxROWS]
//!               [--working-directory=DIR] [--config=PATH] [--hold]
//!               [-e|--] [command args...]
//!   shadow-term --check-config [--config=PATH]
mod config;
mod keys;
mod pty;
mod render;
mod vt;

use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::{
    globals::registry_queue_init,
    protocol::{wl_data_device::WlDataDevice, wl_data_device_manager::DndAction, wl_data_source::WlDataSource, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    data_device_manager::{
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{CopyPasteSource, DataSourceHandler},
        DataDeviceManagerState, WritePipe,
    },
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{PointerEvent, PointerEventKind, PointerHandler, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT},
        Capability, SeatHandler, SeatState,
    },
    shell::{
        xdg::{
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
            XdgShell,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use smithay_client_toolkit::reexports::client::{Dispatch, Proxy};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

const MIMES: &[&str] = &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];

struct Args {
    app_id: String,
    title: Option<String>,
    size: Option<(usize, usize)>,
    cwd: Option<String>,
    config: std::path::PathBuf,
    hold: bool,
    check: bool,
    command: Vec<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        app_id: "shadow-term".into(),
        title: None,
        size: None,
        cwd: None,
        config: config::default_path(),
        hold: false,
        check: false,
        command: vec![],
    };
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        let (k, v) = match arg.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut val = |v: Option<String>| v.or_else(|| it.next()).unwrap_or_default();
        match k.as_str() {
            "-e" | "--" => {
                a.command = it.by_ref().collect();
                break;
            }
            "--app-id" | "-a" => a.app_id = val(v),
            "--title" | "-T" => a.title = Some(val(v)),
            "--window-size-chars" | "-W" => {
                let s = val(v);
                if let Some((c, r)) = s.split_once('x') {
                    if let (Ok(c), Ok(r)) = (c.parse(), r.parse()) {
                        a.size = Some((c, r));
                    }
                }
            }
            "--working-directory" | "-D" => a.cwd = Some(val(v)),
            "--config" | "-c" => a.config = val(v).into(),
            "--hold" | "-H" => a.hold = true,
            "--check-config" | "-C" => a.check = true,
            "--version" | "-v" => {
                println!("shadow-term {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("{}", include_str!("main.rs").lines().take(8).map(|l| l.trim_start_matches("//!")).collect::<Vec<_>>().join("\n"));
                std::process::exit(0);
            }
            other if other.starts_with('-') => eprintln!("shadow-term: ignoring unknown option {other}"),
            _ => {
                // bare command (no -e), like many terminals accept
                a.command = std::iter::once(arg).chain(it.by_ref()).collect();
                break;
            }
        }
    }
    a
}

struct App {
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    _compositor: CompositorState,
    shm: Shm,
    ddm: DataDeviceManagerState,
    data_device: Option<DataDevice>,
    copy_sources: Vec<CopyPasteSource>,
    copy_text: String,
    pool: SlotPool,
    window: Window,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    modifiers: Modifiers,
    loop_handle: LoopHandle<'static, App>,

    cfg: config::Config,
    fonts: render::Fonts,
    term: vt::Term,
    parser: vte::Parser,
    pty: pty::Pty,

    width: u32,  // logical
    height: u32, // logical
    scale: f64, // preferred scale (fractional when wp_fractional_scale_v1 is available)
    frac: Option<(WpViewport, WpFractionalScaleV1)>,
    configured: bool,
    frame_pending: bool,
    needs_draw: bool,
    focused: bool,
    exit: bool,
    exited: Option<i32>,
    hold: bool,
    scroll_off: usize,
    selection: Option<((u64, usize), (u64, usize))>,
    selecting: bool,
    last_click: Option<(Instant, u64, usize)>,
    pointer_pos: (f64, f64),
    buttons_down: u32,
    last_serial: u32,
    started: Instant,
    bell_at: Option<Instant>,
    user_host: String,
    cwd: String,
    foreground: String,
    shell_name: String,
    title_override: Option<String>,
    zoom: f32,
    blink_on: bool,
    /// timer-driven repaint (clock / blink / pulse) that only touches the HUD strip,
    /// the cursor cell and the gutter: damage just those so the compositor recomposites little
    cheap_draw: bool,
    last_clock: u64,
    last_input: Instant,
}

impl App {
    fn phys_size(&self) -> (i32, i32) {
        ((self.width as f64 * self.scale).round() as i32, (self.height as f64 * self.scale).round() as i32)
    }

    fn grid_size(&self) -> (usize, usize) {
        let v = render::layout(&self.cfg, &self.fonts, self.scale);
        let (w, h) = self.phys_size();
        let cols = ((w - v.x0 - v.pad).max(0) / self.fonts.cell_w as i32).max(2) as usize;
        let rows = ((h - v.y0 - v.pad).max(0) / self.fonts.cell_h as i32).max(2) as usize;
        (cols, rows)
    }

    fn logical_size_for(&self, cols: usize, rows: usize) -> (u32, u32) {
        let v = render::layout(&self.cfg, &self.fonts, self.scale);
        let w = v.x0 + v.pad + cols as i32 * self.fonts.cell_w as i32;
        let h = v.y0 + v.pad + rows as i32 * self.fonts.cell_h as i32;
        ((w as f64 / self.scale).round() as u32, (h as f64 / self.scale).round() as u32)
    }

    fn apply_size(&mut self) {
        let (cols, rows) = self.grid_size();
        if cols != self.term.cols || rows != self.term.rows {
            self.term.resize(cols, rows);
            self.pty.resize(cols as u16, rows as u16, (cols * self.fonts.cell_w) as u16, (rows * self.fonts.cell_h) as u16);
            self.scroll_off = 0;
        }
        self.needs_draw = true;
    }

    fn set_font_px(&mut self) {
        let px = self.cfg.font_size * self.zoom * 96.0 / 72.0 * self.scale as f32;
        self.fonts.set_px(px.max(4.0));
        self.apply_size();
    }

    fn write_pty(&mut self, data: &[u8]) {
        self.pty.write_all(data);
    }

    fn handle_pty_output(&mut self) {
        let mut buf = [0u8; 65536];
        let mut total = 0;
        loop {
            match self.pty.read(&mut buf) {
                Ok(0) => {
                    self.child_exited();
                    break;
                }
                Ok(n) => {
                    self.parser.advance(&mut self.term, &buf[..n]);
                    total += n;
                    if total > 1 << 20 {
                        break; // let the UI breathe during floods
                    }
                }
                Err(_) => break,
            }
        }
        if !self.term.responses.is_empty() {
            let r = std::mem::take(&mut self.term.responses);
            self.write_pty(&r);
        }
        for ev in std::mem::take(&mut self.term.events) {
            match ev {
                vt::Event::Title(t) => {
                    if self.title_override.is_none() {
                        self.window.set_title(if t.is_empty() { "shadow-term".to_string() } else { t });
                    }
                }
                vt::Event::Bell => self.bell_at = Some(Instant::now()),
                vt::Event::Clipboard(text) => self.copy_to_clipboard(text),
            }
        }
        if total > 0 {
            self.needs_draw = true;
        }
    }

    fn child_exited(&mut self) {
        if self.exited.is_some() {
            return;
        }
        let code = self.pty.try_wait().unwrap_or(0);
        if self.hold || code != 0 && self.started.elapsed() < Duration::from_secs(2) {
            // keep the window so a failing command's message stays readable
            self.exited = Some(code);
            self.needs_draw = true;
        } else {
            self.exit = true;
        }
    }

    fn copy_to_clipboard(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.copy_text = text;
        if let Some(dd) = &self.data_device {
            let src = self.ddm.create_copy_paste_source(&self.loop_qh(), MIMES.to_vec());
            src.set_selection(dd, self.last_serial);
            self.copy_sources.push(src);
        }
    }

    fn loop_qh(&self) -> QueueHandle<App> {
        QH.with(|q| q.borrow().clone().expect("queue handle"))
    }

    fn paste(&mut self) {
        let Some(dd) = &self.data_device else { return };
        let Some(offer) = dd.data().selection_offer() else { return };
        let mime = offer.with_mime_types(|m| MIMES.iter().find(|x| m.iter().any(|y| y == *x)).map(|s| s.to_string()));
        let Some(mime) = mime else { return };
        // pasting our own selection: no round-trip needed (avoids a self-deadlock)
        if !self.copy_sources.is_empty() && !self.copy_text.is_empty() {
            let t = self.copy_text.clone();
            self.send_paste(t.as_bytes());
            return;
        }
        let Ok(pipe) = offer.receive(mime) else { return };
        let mut data = Vec::new();
        let _ = self.loop_handle.insert_source(pipe, move |_, f, app: &mut App| {
            let file: &mut std::fs::File = unsafe { f.get_mut() };
            let mut buf = [0u8; 8192];
            match file.read(&mut buf) {
                Ok(0) => {
                    let d = std::mem::take(&mut data);
                    app.send_paste(&d);
                    PostAction::Remove
                }
                Ok(n) => {
                    data.extend_from_slice(&buf[..n]);
                    PostAction::Continue
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted || e.kind() == std::io::ErrorKind::WouldBlock => PostAction::Continue,
                Err(_) => PostAction::Remove,
            }
        });
    }

    fn send_paste(&mut self, data: &[u8]) {
        // never let pasted text inject the bracketed-paste terminator
        let clean: Vec<u8> = String::from_utf8_lossy(data).replace("\x1b[201~", "").replace("\r\n", "\r").replace('\n', "\r").into_bytes();
        if self.term.bracketed_paste {
            self.write_pty(b"\x1b[200~");
            self.write_pty(&clean);
            self.write_pty(b"\x1b[201~");
        } else {
            self.write_pty(&clean);
        }
        self.scroll_off = 0;
        self.needs_draw = true;
    }

    fn cell_at(&self, x: f64, y: f64) -> (usize, usize) {
        let v = render::layout(&self.cfg, &self.fonts, self.scale);
        let px = (x * self.scale) as i32 - v.x0;
        let py = (y * self.scale) as i32 - v.y0;
        let col = (px.max(0) / self.fonts.cell_w as i32) as usize;
        let row = (py.max(0) / self.fonts.cell_h as i32) as usize;
        (col.min(self.term.cols - 1), row.min(self.term.rows - 1))
    }

    fn abs_of_row(&self, row: usize) -> u64 {
        self.term.abs_line(row).saturating_sub(self.scroll_off as u64)
    }

    fn handle_key(&mut self, event: &KeyEvent) {
        if self.exited.is_some() {
            self.exit = true;
            return;
        }
        let m = self.modifiers;
        let sym = event.keysym;
        if m.ctrl && m.shift {
            match sym {
                Keysym::C | Keysym::c => {
                    if let Some((a, b)) = self.selection {
                        let t = self.term.text_between(a, b);
                        self.copy_to_clipboard(t);
                    }
                    return;
                }
                Keysym::V | Keysym::v => return self.paste(),
                Keysym::plus | Keysym::equal | Keysym::KP_Add => {
                    self.zoom = (self.zoom * 1.1).min(4.0);
                    return self.set_font_px();
                }
                Keysym::minus | Keysym::underscore | Keysym::KP_Subtract => {
                    self.zoom = (self.zoom / 1.1).max(0.4);
                    return self.set_font_px();
                }
                Keysym::_0 | Keysym::parenright => {
                    self.zoom = 1.0;
                    return self.set_font_px();
                }
                Keysym::N | Keysym::n => {
                    let exe = std::env::current_exe().unwrap_or_else(|_| "shadow-term".into());
                    let _ = std::process::Command::new(exe).current_dir(&self.cwd).spawn();
                    return;
                }
                _ => {}
            }
        }
        if m.shift && !m.ctrl {
            let page = self.term.rows.saturating_sub(1).max(1);
            let max_off = self.term.scrollback.len();
            match sym {
                Keysym::Page_Up => {
                    self.scroll_off = (self.scroll_off + page).min(max_off);
                    self.needs_draw = true;
                    return;
                }
                Keysym::Page_Down => {
                    self.scroll_off = self.scroll_off.saturating_sub(page);
                    self.needs_draw = true;
                    return;
                }
                Keysym::Home if self.scroll_off > 0 || !self.term.alt_active => {
                    self.scroll_off = max_off;
                    self.needs_draw = true;
                    return;
                }
                Keysym::End if self.scroll_off > 0 => {
                    self.scroll_off = 0;
                    self.needs_draw = true;
                    return;
                }
                _ => {}
            }
        }
        if let Some(bytes) = keys::encode(sym, event.utf8.as_deref(), &m, self.term.app_cursor) {
            self.write_pty(&bytes);
            if self.scroll_off != 0 {
                self.scroll_off = 0;
            }
            self.selection = None;
            self.last_input = Instant::now();
            self.blink_on = true;
            self.needs_draw = true;
        }
    }

    fn mouse_report(&mut self, button: u8, col: usize, row: usize, press: bool, motion: bool) {
        let m = self.modifiers;
        let mut b = button as u32 + (m.shift as u32) * 4 + (m.alt as u32) * 8 + (m.ctrl as u32) * 16;
        if motion {
            b += 32;
        }
        let (x, y) = (col + 1, row + 1);
        if self.term.mouse_sgr {
            let s = format!("\x1b[<{};{};{}{}", b, x, y, if press { 'M' } else { 'm' });
            self.write_pty(s.as_bytes());
        } else if x <= 223 && y <= 223 {
            let b = if press { b } else { 3 + (b & !3) };
            self.write_pty(&[0x1b, b'[', b'M', (32 + b) as u8, (32 + x) as u8, (32 + y) as u8]);
        }
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) {
        if !self.configured {
            return;
        }
        let (w, h) = self.phys_size();
        let (w, h) = (w.max(1), h.max(1));
        let stride = w * 4;
        let (buffer, canvas) = match self.pool.create_buffer(w, h, stride, wl_shm::Format::Argb8888) {
            Ok(b) => b,
            Err(_) => return,
        };
        let view = render::layout(&self.cfg, &self.fonts, self.scale);
        let ui = render::Ui {
            cfg: &self.cfg,
            focused: self.focused,
            scroll_off: self.scroll_off,
            selection: self.selection,
            started: self.started,
            bell_at: self.bell_at,
            user_host: &self.user_host,
            cwd: &self.cwd,
            foreground: &self.foreground,
            shell_name: &self.shell_name,
            exited: self.exited,
            blink_on: self.blink_on,
        };
        let mut cv = render::Canvas { buf: canvas, w: w as usize, h: h as usize };
        render::draw(&mut cv, &mut self.fonts, &self.term, &view, &ui);
        let surface = self.window.wl_surface();
        match &self.frac {
            // Fractional path: buffer is w x h physical px, viewport maps it to the logical size.
            Some((vp, _)) => vp.set_destination(self.width.max(1) as i32, self.height.max(1) as i32),
            None => surface.set_buffer_scale(self.scale as i32),
        }
        if self.cheap_draw && !self.needs_draw && !self.term.dirty && !self.animating() {
            let (cw, ch, s) = (self.fonts.cell_w as i32, self.fonts.cell_h as i32, view.scale);
            surface.damage_buffer(0, 0, w, view.y0); // HUD strip (clock)
            let (cx, cy) = (view.x0 + self.term.cx as i32 * cw, view.y0 + self.term.cy as i32 * ch);
            surface.damage_buffer(cx - 2 * s, cy - 2 * s, 2 * cw + 4 * s, ch + 4 * s); // cursor + glow
            if self.cfg.gutter {
                surface.damage_buffer(0, view.y0, view.x0, h - view.y0); // command pulse
            }
        } else {
            surface.damage_buffer(0, 0, w, h);
        }
        self.cheap_draw = false;
        surface.frame(qh, FrameCallbackData(surface.clone()));
        let _ = buffer.attach_to(surface);
        self.window.commit();
        self.frame_pending = true;
        self.needs_draw = false;
        self.term.dirty = false;
    }

    fn animating(&self) -> bool {
        let age = self.started.elapsed().as_secs_f32();
        (self.cfg.boot && age < 1.3)
            || self.bell_at.map(|t| t.elapsed().as_secs_f32() < 0.4).unwrap_or(false)
    }

    /// A command is running (OSC 133 block open). Its gutter pulse is driven by the
    /// 250 ms tick (4 fps) rather than every vblank, so a long build, ssh or vim
    /// session doesn't keep the terminal and compositor redrawing at 60-120 Hz.
    fn command_running(&self) -> bool {
        self.term.blocks.last().map(|b| b.cmd_line.is_some() && b.end_line.is_none()).unwrap_or(false)
    }
}

thread_local! {
    static QH: std::cell::RefCell<Option<QueueHandle<App>>> = const { std::cell::RefCell::new(None) };
}

fn user_host() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "operator".into());
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "localhost".into());
    format!("{user}@{host}")
}

fn short_home(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    if !home.is_empty() && home != "/" && p.starts_with(&home) {
        format!("~{}", &p[home.len()..])
    } else {
        p.to_string()
    }
}

fn main() {
    let args = parse_args();
    let (cfg, problems) = config::load(&args.config);
    if args.check {
        if problems.is_empty() {
            println!("ok: {}", args.config.display());
            std::process::exit(0);
        }
        for p in &problems {
            eprintln!("{}: {p}", args.config.display());
        }
        std::process::exit(1);
    }
    for p in &problems {
        eprintln!("shadow-term: config: {p} (using default for that setting)");
    }

    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("shadow-term: cannot connect to a Wayland compositor ({e})");
            std::process::exit(1);
        }
    };
    let (globals, queue) = registry_queue_init(&conn).expect("wayland registry");
    let qh = queue.handle();
    QH.with(|q| *q.borrow_mut() = Some(qh.clone()));
    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("event loop");
    let lh = event_loop.handle();
    WaylandSource::new(conn.clone(), queue).insert(lh.clone()).expect("wayland source");

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor missing");
    let xdg = XdgShell::bind(&globals, &qh).expect("xdg_shell missing");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm missing");
    let ddm = DataDeviceManagerState::bind(&globals, &qh).expect("data device manager missing");

    let fonts = match render::Fonts::load(&cfg.font, cfg.font_size * 96.0 / 72.0) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("shadow-term: {e}");
            std::process::exit(1);
        }
    };
    let (cols, rows) = args.size.unwrap_or((cfg.cols, cfg.rows));
    let mut term = vt::Term::new(cols, rows);
    term.max_scrollback = cfg.scrollback;
    term.default_fg = cfg.fg;
    term.default_bg = cfg.bg;
    let pty = match pty::Pty::spawn(&args.command, cols as u16, rows as u16, args.cwd.as_deref(), &[]) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("shadow-term: cannot start shell: {e}");
            std::process::exit(1);
        }
    };
    let shell_name = args.command.first().cloned().unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "sh".into()));
    let shell_name = shell_name.rsplit('/').next().unwrap_or("sh").to_string();

    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title(args.title.clone().unwrap_or_else(|| "shadow-term".into()));
    window.set_app_id(args.app_id.clone());
    window.set_min_size(Some((160, 100)));
    // Fractional scaling (e.g. 1.5 on the Dell 14 Plus): render at the exact
    // preferred scale and let wp_viewporter present it at logical size, so text
    // is crisp instead of being resampled by the compositor.
    let frac = match (globals.bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ()), globals.bind::<WpViewporter, _, _>(&qh, 1..=1, ())) {
        (Ok(fm), Ok(vpr)) => {
            let surf = window.wl_surface();
            Some((vpr.get_viewport(surf, &qh, ()), fm.get_fractional_scale(surf, &qh, ())))
        }
        _ => None,
    };
    window.commit();

    let pool = SlotPool::new(1024 * 768 * 4, &shm).expect("shm pool");
    let mut app = App {
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        _compositor: compositor,
        shm,
        ddm,
        data_device: None,
        copy_sources: vec![],
        copy_text: String::new(),
        pool,
        window,
        keyboard: None,
        pointer: None,
        modifiers: Modifiers::default(),
        loop_handle: lh.clone(),
        cfg,
        fonts,
        term,
        parser: vte::Parser::new(),
        pty,
        width: 0,
        height: 0,
        scale: 1.0,
        frac,
        configured: false,
        frame_pending: false,
        needs_draw: true,
        focused: true,
        exit: false,
        exited: None,
        hold: args.hold,
        scroll_off: 0,
        selection: None,
        selecting: false,
        last_click: None,
        pointer_pos: (0.0, 0.0),
        buttons_down: 0,
        last_serial: 0,
        started: Instant::now(),
        bell_at: None,
        user_host: user_host(),
        cwd: args.cwd.clone().map(|c| short_home(&c)).unwrap_or_else(|| "~".into()),
        foreground: String::new(),
        shell_name,
        title_override: args.title.clone(),
        zoom: 1.0,
        blink_on: true,
        cheap_draw: false,
        last_clock: 0,
        last_input: Instant::now(),
    };
    let (lw, lh_) = app.logical_size_for(cols, rows);
    app.width = lw;
    app.height = lh_;

    // pty output
    let fd: OwnedFd = app.pty.master.try_clone().expect("dup pty");
    lh.insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, _, app: &mut App| {
        app.handle_pty_output();
        Ok(PostAction::Continue)
    })
    .expect("pty source");

    // 250 ms tick: clock, cursor blink, cwd/foreground polling, child reaping
    lh.insert_source(Timer::from_duration(Duration::from_millis(250)), |_, _, app: &mut App| {
        let idle = app.last_input.elapsed().as_millis();
        // blink for 10 s after the last keypress, then hold solid: an idle terminal stops
        // committing frames entirely, so the panel can drop into self-refresh
        let blink = idle < 600 || idle > 10_000 || (idle / 530) % 2 == 0;
        if blink != app.blink_on {
            app.blink_on = blink;
            app.cheap_draw = true;
        }
        if app.focused && app.command_running() && app.cfg.gutter {
            app.cheap_draw = true;
        }
        let secs = app.started.elapsed().as_secs();
        if app.cfg.hud && app.focused && secs != app.last_clock {
            app.last_clock = secs;
            app.cheap_draw = true;
        }
        let cwd = app.term.cwd.clone().or_else(|| app.pty.cwd()).map(|c| short_home(&c)).unwrap_or_default();
        if !cwd.is_empty() && cwd != app.cwd {
            app.cwd = cwd;
            app.needs_draw = true;
        }
        if let Some(fg) = app.pty.foreground() {
            app.foreground = fg;
        }
        if app.exited.is_none() {
            if let Some(code) = app.pty.try_wait() {
                if app.hold {
                    app.exited = Some(code);
                    app.needs_draw = true;
                } else {
                    app.exit = true;
                }
            }
        }
        TimeoutAction::ToDuration(Duration::from_millis(250))
    })
    .expect("timer");

    while !app.exit {
        let timeout = if app.animating() { Duration::from_millis(16) } else { Duration::from_millis(250) };
        if event_loop.dispatch(timeout, &mut app).is_err() {
            break;
        }
        if (app.needs_draw || app.term.dirty || app.animating() || app.cheap_draw) && !app.frame_pending {
            app.draw(&qh);
        }
        let _ = conn.flush();
    }
}

// ====================================================================== handlers
impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, factor: i32) {
        // Integer wl_surface.preferred_buffer_scale; ignored once fractional scale is bound.
        if self.frac.is_none() && factor > 0 && factor as f64 != self.scale {
            self.scale = factor as f64;
            self.set_font_px();
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.frame_pending = false;
        if self.needs_draw || self.term.dirty || self.animating() {
            self.draw(qh);
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }
    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &Window, c: WindowConfigure, _: u32) {
        if let (Some(w), Some(h)) = c.new_size {
            self.width = w.get();
            self.height = h.get();
        }
        self.configured = true;
        self.apply_size();
        if !self.frame_pending {
            self.draw(qh);
        }
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat, cap: Capability) {
        if self.data_device.is_none() {
            self.data_device = Some(self.ddm.get_data_device(qh, &seat));
        }
        if cap == Capability::Keyboard && self.keyboard.is_none() {
            let kb = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|app: &mut App, _kbd, event| app.handle_key(&event)),
            );
            if let Ok(k) = kb {
                self.keyboard = Some(k);
            }
        }
        if cap == Capability::Pointer && self.pointer.is_none() {
            if let Ok(p) = self.seat_state.get_pointer(qh, &seat) {
                self.pointer = Some(p);
            }
        }
    }
    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, cap: Capability) {
        if cap == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
        }
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, serial: u32, _: &[u32], _: &[Keysym]) {
        self.focused = true;
        self.last_serial = serial;
        if self.term.focus_events {
            self.write_pty(b"\x1b[I");
        }
        self.needs_draw = true;
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: &wl_surface::WlSurface, _: u32) {
        self.focused = false;
        if self.term.focus_events {
            self.write_pty(b"\x1b[O");
        }
        self.needs_draw = true;
    }
    fn press_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, serial: u32, event: KeyEvent) {
        self.last_serial = serial;
        self.handle_key(&event);
    }
    fn repeat_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, event: KeyEvent) {
        self.handle_key(&event);
    }
    fn release_key(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, _: KeyEvent) {}
    fn update_modifiers(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_keyboard::WlKeyboard, _: u32, m: Modifiers, _: RawModifiers, _: u32) {
        self.modifiers = m;
    }
}

impl PointerHandler for App {
    fn pointer_frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, events: &[PointerEvent]) {
        for ev in events {
            if &ev.surface != self.window.wl_surface() {
                continue;
            }
            self.pointer_pos = ev.position;
            let (col, row) = self.cell_at(ev.position.0, ev.position.1);
            let reporting = self.term.mouse != vt::MouseMode::Off && !self.modifiers.shift;
            match ev.kind {
                PointerEventKind::Press { button, serial, .. } => {
                    self.last_serial = serial;
                    self.buttons_down |= 1 << (button - BTN_LEFT).min(31);
                    let b = match button {
                        BTN_LEFT => 0,
                        BTN_MIDDLE => 1,
                        BTN_RIGHT => 2,
                        _ => continue,
                    };
                    if reporting {
                        self.mouse_report(b, col, row, true, false);
                        continue;
                    }
                    match button {
                        BTN_LEFT => {
                            let abs = self.abs_of_row(row);
                            let double = self.last_click.map(|(t, l, c)| t.elapsed() < Duration::from_millis(400) && l == abs && c == col).unwrap_or(false);
                            if double {
                                // select the word under the pointer
                                if let Some(line) = self.term.line_at(abs) {
                                    let is_word = |c: char| c.is_alphanumeric() || "_-./~:@".contains(c);
                                    let mut a = col;
                                    let mut b = col;
                                    while a > 0 && is_word(line[a - 1].ch) {
                                        a -= 1;
                                    }
                                    while b + 1 < line.len() && is_word(line[b + 1].ch) {
                                        b += 1;
                                    }
                                    self.selection = Some(((abs, a), (abs, b)));
                                }
                                self.selecting = false;
                            } else {
                                self.selection = Some(((abs, col), (abs, col)));
                                self.selecting = true;
                            }
                            self.last_click = Some((Instant::now(), abs, col));
                            self.needs_draw = true;
                        }
                        BTN_MIDDLE => self.paste(),
                        _ => {}
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    self.buttons_down &= !(1 << (button - BTN_LEFT).min(31));
                    if reporting {
                        let b = match button {
                            BTN_LEFT => 0,
                            BTN_MIDDLE => 1,
                            BTN_RIGHT => 2,
                            _ => continue,
                        };
                        self.mouse_report(b, col, row, false, false);
                        continue;
                    }
                    if button == BTN_LEFT {
                        self.selecting = false;
                        if let Some((a, b)) = self.selection {
                            if a == b {
                                self.selection = None;
                                self.needs_draw = true;
                            }
                        }
                    }
                }
                PointerEventKind::Motion { .. } => {
                    if reporting {
                        let drag = self.buttons_down != 0 && matches!(self.term.mouse, vt::MouseMode::Drag | vt::MouseMode::Motion);
                        if drag || self.term.mouse == vt::MouseMode::Motion {
                            let b = if self.buttons_down & 1 != 0 { 0 } else if self.buttons_down & 4 != 0 { 1 } else if self.buttons_down & 2 != 0 { 2 } else { 3 };
                            self.mouse_report(b, col, row, true, true);
                        }
                        continue;
                    }
                    if self.selecting {
                        let abs = self.abs_of_row(row);
                        if let Some((a, _)) = self.selection {
                            self.selection = Some((a, (abs, col)));
                            self.needs_draw = true;
                        }
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let steps = if vertical.discrete != 0 {
                        vertical.discrete
                    } else if vertical.value120 != 0 {
                        vertical.value120 / 120
                    } else {
                        (vertical.absolute / 15.0).round() as i32
                    };
                    if steps == 0 {
                        continue;
                    }
                    if reporting {
                        for _ in 0..steps.abs() {
                            self.mouse_report(if steps < 0 { 64 } else { 65 }, col, row, true, false);
                        }
                    } else if self.term.alt_active {
                        let seq: &[u8] = if steps < 0 { b"\x1b[A" } else { b"\x1b[B" };
                        for _ in 0..steps.abs() * 3 {
                            self.write_pty(seq);
                        }
                    } else {
                        let lines = steps.unsigned_abs() as usize * 3;
                        if steps < 0 {
                            self.scroll_off = (self.scroll_off + lines).min(self.term.scrollback.len());
                        } else {
                            self.scroll_off = self.scroll_off.saturating_sub(lines);
                        }
                        self.needs_draw = true;
                    }
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl DataDeviceHandler for App {
    fn enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64, _: &wl_surface::WlSurface) {}
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataOfferHandler for App {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

impl DataSourceHandler for App {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    fn send_request(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource, _mime: String, pipe: WritePipe) {
        if self.copy_sources.iter().any(|s| s.inner() == source) {
            let mut f = std::fs::File::from(OwnedFd::from(pipe));
            let _ = f.write_all(self.copy_text.as_bytes());
        }
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        self.copy_sources.retain(|s| s.inner() != source);
        source.destroy();
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

smithay_client_toolkit::delegate_dispatch2!(App);

impl Dispatch<WpFractionalScaleManagerV1, ()> for App {
    fn event(_: &mut Self, _: &WpFractionalScaleManagerV1, _: <WpFractionalScaleManagerV1 as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpViewporter, ()> for App {
    fn event(_: &mut Self, _: &WpViewporter, _: <WpViewporter as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpViewport, ()> for App {
    fn event(_: &mut Self, _: &WpViewport, _: <WpViewport as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<WpFractionalScaleV1, ()> for App {
    fn event(app: &mut Self, _: &WpFractionalScaleV1, ev: wp_fractional_scale_v1::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = ev {
            let s = (scale as f64 / 120.0).clamp(0.5, 4.0);
            if (s - app.scale).abs() > f64::EPSILON {
                app.scale = s;
                app.set_font_px();
                if app.configured && !app.frame_pending {
                    app.draw(qh);
                }
            }
        }
    }
}

#[allow(dead_code)]
fn _assert_fd<T: AsFd>(_: &T) {}
