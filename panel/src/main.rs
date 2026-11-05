//! shadow-panel — the shell's windows (10.0): a taskbar along the bottom of every screen, and the
//! start menu, widgets (left) and quick settings (right) as flyouts over them.
//!
//! Each is a page served by shadow-webd on 127.0.0.1, shown in a frameless WebKitGTK view inside a
//! layer-shell surface (the compositor docks it: the taskbar's exclusive zone keeps the app area
//! clear of it). Nothing gives away that it is a web page: no address bar, no context menu, no
//! zoom, no scrollbars, no navigation away from the local origin, no new windows, no developer
//! tools, and the browsing data lives in $XDG_RUNTIME_DIR (gone at logout).
//!
//!   shadow-panel                     run (started by shadow-shell with the session)
//!   shadow-panel toggle ROLE         start | widgets | quick: open it, or close it if open
//!   shadow-panel show ROLE | hide    open one / close the open flyout
//!   shadow-panel reload              reload every page (after shadow-webd restarted)
use gtk::prelude::*;
use gtk::{gdk, glib};
use gtk_layer_shell::{self as ls, LayerShell};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::rc::Rc;
use wry::{WebContext, WebView, WebViewBuilder, WebViewBuilderExtUnix};

/// taskbar height, logical px (the page is laid out for it)
const TASKBAR_H: i32 = 48;
const ROLES: [&str; 3] = ["start", "widgets", "quick"];

fn runtime() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}
fn sock_path() -> PathBuf {
    runtime().join("shadow-panel.sock")
}
fn key_path() -> PathBuf {
    runtime().join("shadow-web/key")
}
fn port() -> u16 {
    // the same per-user port as shadow-webd
    47000 + (unsafe { libc::getuid() } % 1000) as u16
}
fn base() -> String {
    format!("http://127.0.0.1:{}", port())
}
fn read_key() -> Option<String> {
    std::fs::read_to_string(key_path()).ok().map(|k| k.trim().to_string()).filter(|k| k.len() == 64 && k.chars().all(|c| c.is_ascii_hexdigit()))
}
fn page(role: &str, screen: &str, key: &str) -> String {
    let enc: String = screen.chars().filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c)).collect();
    format!("{}/shell?role={role}&screen={enc}#k={key}", base())
}

/// A message for the running panel. 0 = delivered.
fn send(line: &str) -> i32 {
    match UnixStream::connect(sock_path()) {
        Ok(mut s) => {
            let _ = s.write_all(format!("{line}\n").as_bytes());
            0
        }
        Err(_) => {
            eprintln!("shadow-panel is not running");
            1
        }
    }
}

/// the monitor's connector name (the compositor puts it in the output's model)
fn mon_name(m: &gdk::Monitor) -> String {
    m.model().map(|s| s.to_string()).unwrap_or_default()
}

/// the screen with the keyboard, from the compositor
fn focused_screen() -> Option<String> {
    let o = shadow_niri::compositor_command("shell").ok()?;
    let v: serde_json::Value = serde_json::from_str(&o).ok()?;
    v["screens"].as_array()?.iter().find(|s| s["focused"].as_bool() == Some(true)).and_then(|s| s["name"].as_str().map(String::from))
}

struct View {
    win: gtk::Window,
    web: WebView,
    screen: String,
}

struct Panel {
    key: String,
    ctx: WebContext,
    taskbars: Vec<(gdk::Monitor, View)>,
    flyouts: HashMap<(String, String), View>,
    open: Option<(String, String)>,
}

type Shared = Rc<RefCell<Panel>>;

/// a transparent, frameless layer-shell window holding one web view
fn make(sh: &Shared, role: &str, mon: &gdk::Monitor) -> Option<View> {
    let win = gtk::Window::new(gtk::WindowType::Toplevel);
    win.set_decorated(false);
    win.set_app_paintable(true);
    if let Some(v) = gtk::prelude::WidgetExt::screen(&win).and_then(|s| s.rgba_visual()) {
        win.set_visual(Some(&v));
    }
    win.init_layer_shell();
    <gtk::Window as LayerShell>::set_monitor(&win, mon);
    let geo = mon.geometry();
    let (mw, mh) = (geo.width(), geo.height());
    match role {
        "taskbar" => {
            <gtk::Window as LayerShell>::set_layer(&win, ls::Layer::Top);
            <gtk::Window as LayerShell>::set_namespace(&win, "shadow-taskbar");
            for e in [ls::Edge::Left, ls::Edge::Right, ls::Edge::Bottom] {
                <gtk::Window as LayerShell>::set_anchor(&win, e, true);
            }
            <gtk::Window as LayerShell>::set_exclusive_zone(&win, TASKBAR_H);
            <gtk::Window as LayerShell>::set_keyboard_interactivity(&win, false);
            win.set_size_request(-1, TASKBAR_H);
        }
        _ => {
            <gtk::Window as LayerShell>::set_layer(&win, ls::Layer::Overlay);
            <gtk::Window as LayerShell>::set_namespace(&win, &format!("shadow-flyout-{role}"));
            <gtk::Window as LayerShell>::set_keyboard_interactivity(&win, true);
            let gap = 10;
            let (w, h) = match role {
                "start" => (660.min(mw - 2 * gap), 680.min(mh - TASKBAR_H - 2 * gap)),
                "widgets" => (420.min(mw - 2 * gap), 820.min(mh - TASKBAR_H - 2 * gap)),
                _ => (380.min(mw - 2 * gap), 500.min(mh - TASKBAR_H - 2 * gap)),
            };
            <gtk::Window as LayerShell>::set_anchor(&win, ls::Edge::Bottom, true);
            win.set_layer_shell_margin(ls::Edge::Bottom, TASKBAR_H + gap);
            match role {
                "widgets" => {
                    <gtk::Window as LayerShell>::set_anchor(&win, ls::Edge::Left, true);
                    win.set_layer_shell_margin(ls::Edge::Left, gap);
                }
                "quick" => {
                    <gtk::Window as LayerShell>::set_anchor(&win, ls::Edge::Right, true);
                    win.set_layer_shell_margin(ls::Edge::Right, gap);
                }
                _ => {} // start: centred above the taskbar
            }
            win.set_size_request(w.max(200), h.max(200));
        }
    }
    let boxed = gtk::Box::new(gtk::Orientation::Vertical, 0);
    win.add(&boxed);
    let screen = mon_name(mon);
    let origin = base();
    let (sh2, role2, screen2) = (sh.clone(), role.to_string(), screen.clone());
    let key = sh.borrow().key.clone();
    let web = {
        let mut p = sh.borrow_mut();
        let b = WebViewBuilder::new_with_web_context(&mut p.ctx)
            .with_url(page(role, &screen, &key))
            .with_transparent(true)
            .with_background_color((0, 0, 0, 0))
            .with_devtools(false)
            .with_back_forward_navigation_gestures(false)
            .with_user_agent("Shadow")
            // the page may only ever be our own local origin
            .with_navigation_handler(move |u| u.starts_with(&origin))
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_initialization_script("document.addEventListener('contextmenu',e=>e.preventDefault(),true);")
            .with_ipc_handler(move |req| {
                let body = req.body().clone();
                let (sh, role, screen) = (sh2.clone(), role2.clone(), screen2.clone());
                // after the handler returns: it may create or hide windows
                glib::idle_add_local_once(move || from_page(&sh, &role, &screen, &body));
            });
        match b.build_gtk(&boxed) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("shadow-panel: {role} on {screen}: {e}");
                return None;
            }
        }
    };
    Some(View { win, web, screen })
}

/// what a page asked for (window.ipc.postMessage)
fn from_page(sh: &Shared, role: &str, screen: &str, body: &str) {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let want = v["role"].as_str().unwrap_or("");
    match v["do"].as_str().unwrap_or("") {
        "toggle" if ROLES.contains(&want) => toggle(sh, want, Some(screen.to_string())),
        "show" if ROLES.contains(&want) => show(sh, want, Some(screen.to_string())),
        "hide" => hide(sh),
        _ => eprintln!("shadow-panel: {role}: unknown request {body}"),
    }
}

fn monitor_named(name: &str) -> Option<gdk::Monitor> {
    let d = gdk::Display::default()?;
    (0..d.n_monitors()).filter_map(|i| d.monitor(i)).find(|m| mon_name(m) == name).or_else(|| d.monitor(0))
}

fn show(sh: &Shared, role: &str, screen: Option<String>) {
    let screen = screen.or_else(focused_screen).or_else(|| gdk::Display::default().and_then(|d| d.monitor(0)).map(|m| mon_name(&m))).unwrap_or_default();
    hide(sh);
    let k = (role.to_string(), screen.clone());
    if !sh.borrow().flyouts.contains_key(&k) {
        let Some(m) = monitor_named(&screen) else { return };
        let Some(v) = make(sh, role, &m) else { return };
        sh.borrow_mut().flyouts.insert(k.clone(), v);
    }
    let p = sh.borrow();
    if let Some(v) = p.flyouts.get(&k) {
        v.win.show_all();
        let _ = v.web.evaluate_script("window.shellShow && window.shellShow()");
        let _ = v.web.focus();
    }
    drop(p);
    sh.borrow_mut().open = Some(k);
}

fn hide(sh: &Shared) {
    let open = sh.borrow_mut().open.take();
    if let Some(k) = open {
        if let Some(v) = sh.borrow().flyouts.get(&k) {
            v.win.hide();
        }
    }
}

fn toggle(sh: &Shared, role: &str, screen: Option<String>) {
    let open = sh.borrow().open.clone();
    match open {
        Some((r, _)) if r == role => hide(sh),
        _ => show(sh, role, screen),
    }
}

/// a taskbar on every monitor (and none on one that went away)
fn sync_taskbars(sh: &Shared) {
    let Some(d) = gdk::Display::default() else { return };
    let mons: Vec<gdk::Monitor> = (0..d.n_monitors()).filter_map(|i| d.monitor(i)).collect();
    let gone: Vec<usize> = sh.borrow().taskbars.iter().enumerate().filter(|(_, (m, _))| !mons.contains(m)).map(|(i, _)| i).collect();
    for i in gone.into_iter().rev() {
        let (_, v) = sh.borrow_mut().taskbars.remove(i);
        let screen = v.screen.clone();
        v.win.close();
        sh.borrow_mut().flyouts.retain(|(_, s), f| {
            if *s == screen {
                f.win.close();
                false
            } else {
                true
            }
        });
    }
    for m in mons {
        if sh.borrow().taskbars.iter().any(|(x, _)| *x == m) {
            continue;
        }
        if let Some(v) = make(sh, "taskbar", &m) {
            v.win.show_all();
            sh.borrow_mut().taskbars.push((m, v));
        }
    }
}

fn reload(sh: &Shared) {
    let Some(key) = read_key() else { return };
    let mut p = sh.borrow_mut();
    p.key = key.clone();
    for (_, v) in &p.taskbars {
        let _ = v.web.load_url(&page("taskbar", &v.screen, &key));
    }
    for ((role, _), v) in &p.flyouts {
        let _ = v.web.load_url(&page(role, &v.screen, &key));
    }
}

fn command(sh: &Shared, line: &str) {
    let mut w = line.split_whitespace();
    match (w.next(), w.next()) {
        (Some("toggle"), Some(r)) if ROLES.contains(&r) => toggle(sh, r, None),
        (Some("show"), Some(r)) if ROLES.contains(&r) => show(sh, r, None),
        (Some("hide"), _) => hide(sh),
        (Some("reload"), _) => reload(sh),
        // the compositor's shell keys: the taskbar on/off
        (Some("toggle"), Some("toggle")) => {
            for (_, v) in &sh.borrow().taskbars {
                if v.win.is_visible() { v.win.hide() } else { v.win.show_all() }
            }
        }
        _ => eprintln!("shadow-panel: unknown command: {line}"),
    }
}

fn run() -> i32 {
    // one per session: a live socket means one runs already
    let sp = sock_path();
    if UnixStream::connect(&sp).is_ok() {
        eprintln!("shadow-panel: already running");
        return 0;
    }
    let _ = std::fs::remove_file(&sp);
    let listener = match UnixListener::bind(&sp) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("shadow-panel: {}: {e}", sp.display());
            return 1;
        }
    };
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&sp, std::fs::Permissions::from_mode(0o600));
    let _ = listener.set_nonblocking(true);
    // shadow-webd starts with the session too: give it a moment
    let mut key = read_key();
    for _ in 0..50 {
        if key.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        key = read_key();
    }
    let Some(key) = key else {
        eprintln!("shadow-panel: shadow-webd is not running (no key in {})", key_path().display());
        return 3;
    };
    if gtk::init().is_err() {
        eprintln!("shadow-panel: no display");
        return 3;
    }
    if !ls::is_supported() {
        eprintln!("shadow-panel: the compositor has no layer-shell");
        return 3;
    }
    let data = runtime().join("shadow-panel");
    let _ = std::fs::create_dir_all(&data);
    let sh: Shared = Rc::new(RefCell::new(Panel { key, ctx: WebContext::new(Some(data)), taskbars: vec![], flyouts: HashMap::new(), open: None }));
    sync_taskbars(&sh);
    if sh.borrow().taskbars.is_empty() {
        eprintln!("shadow-panel: no taskbar could be created");
        return 3;
    }
    if let Some(d) = gdk::Display::default() {
        let s1 = sh.clone();
        d.connect_monitor_added(move |_, _| sync_taskbars(&s1));
        let s2 = sh.clone();
        d.connect_monitor_removed(move |_, _| {
            let s = s2.clone();
            glib::idle_add_local_once(move || sync_taskbars(&s));
        });
    }
    // commands from the compositor's keys and from shadowctl
    let s3 = sh.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(40), move || {
        while let Ok((s, _)) = listener.accept() {
            let _ = s.set_nonblocking(false);
            let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(200)));
            let mut line = String::new();
            if BufReader::new(&s).read_line(&mut line).is_ok() {
                command(&s3, line.trim());
            }
        }
        glib::ControlFlow::Continue
    });
    // shadow-webd restarted (a new key): every page reloads with it
    let s4 = sh.clone();
    glib::timeout_add_seconds_local(5, move || {
        if read_key().is_some_and(|k| k != s4.borrow().key) {
            reload(&s4);
        }
        glib::ControlFlow::Continue
    });
    gtk::main();
    0
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
    let rc = match a.as_slice() {
        [] | ["run"] => run(),
        ["toggle", r] | ["show", r] => send(&format!("{} {r}", a[0])),
        ["hide"] | ["reload"] => send(a[0]),
        _ => {
            eprintln!("usage: shadow-panel [toggle start|widgets|quick | show ROLE | hide | reload]");
            2
        }
    };
    std::process::exit(rc);
}
