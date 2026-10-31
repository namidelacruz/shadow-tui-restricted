//! shadow-lock — the session lock (ext-session-lock-v1): one static screen per monitor, PAM
//! authentication (service "shadow-lock") on a worker thread. Started by Mod+Alt+L, by idle
//! (shadow-displayd) and on lid close when not docked. Media and brightness keys keep working.
use shadow_apps::pam;
use shadow_ui::calloop::channel::{self, Sender};
use shadow_ui::widgets::Input;
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};

struct Lock {
    tx: Sender<String>,
    user: String,
    host: String,
    pass: Input,
    msg: (String, bool),
    checking: bool,
    failures: u32,
}

fn user() -> String {
    std::env::var("USER").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| {
        // SAFETY: getpwuid on our own uid; the returned pointer is read immediately
        unsafe {
            let pw = libc::getpwuid(libc::getuid());
            if pw.is_null() {
                return "user".into();
            }
            std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned()
        }
    })
}

impl App for Lock {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Lock
    }

    fn lock_state(&mut self, locked: bool, ctx: &mut Ctx) {
        if !locked {
            eprintln!("shadow-lock: the compositor refused the lock (already locked?)");
            ctx.quit();
        }
    }

    fn message(&mut self, m: String, ctx: &mut Ctx) {
        self.checking = false;
        if m == "ok" {
            ctx.unlock();
            return;
        }
        self.failures += 1;
        self.msg = (m.strip_prefix("fail:").unwrap_or("access denied").to_string(), true);
        ctx.redraw();
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        if self.checking {
            return;
        }
        ctx.redraw();
        match k.sym {
            Keysym::Return | Keysym::KP_Enter => {
                if self.pass.text.is_empty() {
                    return;
                }
                let pw = std::mem::take(&mut self.pass.text);
                self.pass.cur = 0;
                self.checking = true;
                self.msg = ("verifying".into(), false);
                let (tx, user) = (self.tx.clone(), self.user.clone());
                std::thread::spawn(move || {
                    let demo = std::env::var("SHADOW_LOCK_DEMO").is_ok_and(|v| v == "1") && pw == "shadow";
                    let r = if demo { Ok(()) } else { pam::authenticate("shadow-lock", &user, &pw) };
                    let _ = tx.send(match r {
                        Ok(()) => "ok".into(),
                        Err(e) => format!("fail:{e}"),
                    });
                });
            }
            Keysym::Escape => {
                self.pass.clear();
                self.msg.0.clear();
            }
            _ => {
                self.pass.key(k);
            }
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        f.cv.clear(th.bg, 1.0);
        let pt = th.font_pt * 1.15;
        let lh = f.line_h(pt);
        let bw = f.px(520.0).min(w - f.px(40.0));
        let bh = lh * 5 + f.px(64.0);
        let (x, y) = ((w - bw) / 2, (h - bh) / 2);
        f.cv.fill(x, y, bw, bh, th.panel, 1.0);
        let hair = f.hair();
        f.cv.border(x, y, bw, bh, hair, th.amber, 0.8);
        f.cv.brackets(x - f.px(6.0), y - f.px(6.0), bw + f.px(12.0), bh + f.px(12.0), f.px(18.0), f.px(2.0), th.amber, 1.0);
        let pad = f.px(22.0);
        let tag = " ◢ SHADOW TUI // LOCKED ";
        let tw = f.text_w(tag, pt, true);
        f.cv.fill(x + pad, y + pad, tw, lh + f.px(6.0), th.amber, 1.0);
        f.text(x + pad, y + pad + f.px(3.0), tag, pt, th.bg, true);
        let ly = y + pad + lh + f.px(22.0);
        f.text(x + pad, ly, "operator", pt * 0.9, th.dim, false);
        let id = format!("{}@{}", self.user, self.host);
        f.text(x + pad + f.px(110.0), ly, &id, pt, th.cyan, false);
        let py = ly + lh + f.px(14.0);
        f.text(x + pad, py, "password", pt * 0.9, th.amber, true);
        let fx = x + pad + f.px(110.0);
        f.cv.fill(fx - f.px(6.0), py - f.px(3.0), bw - (fx - x) - pad + f.px(6.0), lh + f.px(6.0), th.bg, 1.0);
        self.pass.draw(f, fx, py, pt, !self.checking);
        if !self.msg.0.is_empty() {
            let m = f.fit(&self.msg.0, pt * 0.9, false, bw - 2 * pad);
            f.text(x + pad, py + lh + f.px(18.0), &m, pt * 0.9, if self.msg.1 { th.crimson } else { th.cyan }, self.msg.1);
        }
    }
}

fn main() {
    let host = std::fs::read_to_string("/etc/hostname").or_else(|_| std::fs::read_to_string("/proc/sys/kernel/hostname")).map(|s| s.trim().to_string()).unwrap_or_else(|_| "localhost".into());
    let (tx, rx) = channel::channel::<String>();
    let l = Lock { tx, user: user(), host, pass: Input::secret(), msg: (String::new(), false), checking: false, failures: 0 };
    if let Err(e) = run(Box::new(l), Some(rx)) {
        eprintln!("shadow-lock: {e}");
        std::process::exit(1);
    }
}
