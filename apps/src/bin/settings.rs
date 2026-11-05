//! shadow-settings [PAGE] — open Shadow's settings web app (9.0) on a page:
//!   settings · fix · defaults · config · crash      (look = settings)
//! Starts shadow-webd if it isn't running, then opens the app as its own window: a Chromium-family
//! browser in app mode (--app), else a Firefox window, else the default browser. With no browser
//! at all, the native windows open instead (shadow-config, shadow-fix, shadow-prefs) — settings
//! never become unreachable.
use std::process::{Command, Stdio};

fn have(c: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(c).is_file()))
}

fn url(page: &str) -> Option<String> {
    let o = Command::new("shadow-webd").args(["url", page]).stderr(Stdio::null()).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn detach(cmd: &mut Command) -> bool {
    use std::os::unix::process::CommandExt;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn().is_ok()
}

fn native(page: &str) {
    let prog = match page {
        "fix" => "shadow-fix",
        "defaults" | "crash" => "shadow-prefs",
        _ => "shadow-config",
    };
    detach(&mut Command::new(prog));
}

fn main() {
    let page = std::env::args().nth(1).unwrap_or_else(|| "settings".into());
    let page = match page.as_str() {
        "look" | "settings" => "settings",
        "fix" | "defaults" | "config" | "crash" => page.as_str(),
        _ => "settings",
    }
    .to_string();
    // the service: started with the session; start it now if it isn't
    let mut u = url(&page);
    if u.is_none() && have("shadow-webd") {
        detach(&mut Command::new("shadow-webd"));
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            u = url(&page);
            if u.is_some() {
                break;
            }
        }
    }
    let Some(u) = u else {
        eprintln!("shadow-settings: the settings service is not available — opening the native window");
        return native(&page);
    };
    // its own window: app mode where the browser has it
    for b in ["chromium", "chromium-browser", "google-chrome-stable", "google-chrome", "brave-browser", "brave", "vivaldi-stable", "microsoft-edge-stable"] {
        if have(b) && detach(Command::new(b).arg(format!("--app={u}")).arg("--class=shadow-settings").arg("--ozone-platform-hint=auto")) {
            return;
        }
    }
    for b in ["firefox", "librewolf", "zen-browser"] {
        if have(b) && detach(Command::new(b).args(["--new-window", &u])) {
            return;
        }
    }
    if have("xdg-open") && detach(Command::new("xdg-open").arg(&u)) {
        return;
    }
    native(&page);
}
