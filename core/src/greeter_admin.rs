//! `shadow greeter …` — manage the login screen (greetd). Port of greeter_admin.py.
//! Changes take effect at the next boot, so a broken greeter can never kill the running session.
use crate::st::Paths;
use crate::sys;
use std::path::Path;

const GREETD_CONF: &str = "/etc/greetd/config.toml";
const BACKUP: &str = "/etc/greetd/config.toml.shadow-tui-bak";
const ND_CONF: &str = "/etc/shadow-tui/greeter.conf";
const PREV_DM: &str = "/etc/shadow-tui/previous-dm";
const WRAPPER: &str = "/opt/shadow-tui/lib/greeter-wrapper";
const DM_LINK: &str = "/etc/systemd/system/display-manager.service";
const USAGE: &str = "usage: shadow greeter enable [--warning TEXT | --no-warning] [--font auto|off|NAME] | disable | status\n                         | preview | warning TEXT | warning --off | warning (reset)";

fn need_root(args: &[&str]) -> Result<(), i32> {
    if sys::is_root() {
        return Ok(());
    }
    use std::os::unix::process::CommandExt;
    let exe = sys::which("shadow").map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| std::env::args().next().unwrap_or_default());
    let e = std::process::Command::new("sudo").arg(exe).arg("greeter").args(args).exec();
    eprintln!("sudo: {e}");
    Err(1)
}

fn fail(m: &str) -> i32 {
    eprintln!("{m}");
    1
}

fn sh(args: &[&str]) -> (i32, String) {
    let (c, o, e) = sys::run(args);
    (c, format!("{o}\n{e}").trim().to_string())
}

fn read(p: &str) -> Option<String> {
    std::fs::read_to_string(sys::p(p)).ok()
}

fn current_dm() -> Option<String> {
    sys::link_name(DM_LINK)
}

fn flag_val<'a>(args: &[&'a str], f: &str) -> Option<&'a str> {
    args.iter().position(|a| *a == f).and_then(|i| args.get(i + 1)).copied()
}

fn conf_value(key: &str) -> Option<String> {
    read(ND_CONF)?.lines().find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key).map(|(_, v)| v.trim().to_string()))
}

fn font_arg(args: &[&str]) -> String {
    flag_val(args, "--font").map(String::from).or_else(|| conf_value("console_font")).unwrap_or_else(|| "auto".into())
}

fn warning_line(args: &[&str]) -> String {
    if args.contains(&"--no-warning") {
        return "warning=\n".into();
    }
    if let Some(w) = flag_val(args, "--warning") {
        return format!("warning={}\n", w.replace('\n', " "));
    }
    conf_value("warning").map(|c| format!("warning={c}\n")).unwrap_or_default()
}

fn write(p: &str, t: &str) -> std::io::Result<()> {
    let path = sys::p(p);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, t)
}

fn enable(args: &[&str]) -> i32 {
    let mut a = vec!["enable"];
    a.extend_from_slice(args);
    if let Err(c) = need_root(&a) {
        return c;
    }
    if sys::which("greetd").is_none() {
        return fail("greetd is not installed: sudo pacman -S greetd");
    }
    if sys::which("shadow-greet").is_none() {
        return fail("shadow-greet missing — reinstall the shadow-tui-restricted package");
    }
    let font = font_arg(args);
    let (code, out) = sh(&["shadow-greet", "--self-test"]);
    println!("{out}");
    if code != 0 {
        return fail("login screen self-test failed (no desktop session found); nothing changed");
    }
    if font == "auto" && !std::path::Path::new("/usr/share/kbd/consolefonts/ter-v24n.psf.gz").exists() {
        println!("note: terminus-font is not installed — the login screen keeps the default console font size");
    }
    if sys::which("agreety").is_none() {
        println!("warning: agreety (greetd's fallback prompt) not found — fallback will be a short message only");
    }
    let fallback = if sys::which("shadow-session").is_some() { "/opt/shadow-tui/bin/shadow-session" } else if sys::which("niri-session").is_some() { "niri-session" } else { "${SHELL:-/bin/sh} -l" };
    let conf = format!(
        "# Shadow TUI Restricted login screen (shadow-greet: static console login; agreety as fallback)\nsession=shadow-tui\nconsole_font={font}\n# title=SHADOW TUI // RESTRICTED\n# warning=<text in the login box>  (empty = none; absent = AUTHORIZED ACCESS ONLY)\n{}fallback_cmd={fallback}\n",
        warning_line(args)
    );
    if let Err(e) = write(ND_CONF, &conf) {
        return fail(&format!("{ND_CONF}: {e}"));
    }
    let mut vt = "1".to_string();
    if let Some(txt) = read(GREETD_CONF) {
        if let Some(v) = txt.lines().find_map(|l| {
            let l = l.trim();
            l.strip_prefix("vt").map(str::trim_start).and_then(|r| r.strip_prefix('=')).map(|r| r.split_whitespace().next().unwrap_or("1").to_string())
        }) {
            vt = v;
        }
        for legacy in ["/etc/greetd/config.toml.shadownet-bak", "/etc/greetd/config.toml.netdive-bak"] {
        if !sys::exists(BACKUP) && sys::exists(legacy) && std::fs::rename(sys::p(legacy), sys::p(BACKUP)).is_ok() {
            println!("adopted the older backup of the original greetd config ({legacy}) -> {BACKUP}");
        }
        }
        if !sys::exists(BACKUP) && !txt.contains("shadow-tui") && !txt.contains("shadow-network") && !txt.contains("netdive") && std::fs::copy(sys::p(GREETD_CONF), sys::p(BACKUP)).is_ok() {
            println!("backed up {GREETD_CONF} -> {BACKUP}");
        }
    }
    let g = format!("# Managed by `shadow greeter` — `shadow greeter disable` restores the previous file.\n[terminal]\nvt = {vt}\n\n[default_session]\ncommand = \"{WRAPPER}\"\nuser = \"greeter\"\n");
    if let Err(e) = write(GREETD_CONF, &g) {
        return fail(&format!("{GREETD_CONF}: {e}"));
    }
    let cache = sys::p("/var/cache/shadow-tui-greeter");
    let _ = std::fs::create_dir_all(&cache);
    if sys::run(&["id", "-u", "greeter"]).0 == 0 {
        sys::run(&["chown", "greeter", &cache.to_string_lossy()]);
    }
    if let Some(prev) = read("/etc/shadow-network/previous-dm").or_else(|| read("/etc/netdive/previous-dm")) {
        if !sys::exists(PREV_DM) {
            let _ = write(PREV_DM, &prev);
        }
    }
    if let Some(dm) = current_dm().filter(|d| d != "greetd.service") {
        let _ = write(PREV_DM, &format!("{dm}\n"));
        sh(&["systemctl", "disable", &dm]);
        println!("disabled {dm} (remembered for `shadow greeter disable`)");
    }
    let (c, o) = sh(&["systemctl", "enable", "greetd.service"]);
    if c != 0 {
        return fail(&format!("could not enable greetd: {o}"));
    }
    println!("\nShadow TUI Restricted login screen enabled — it appears at the next boot.");
    println!("Preview it now in any terminal:  shadow greeter preview");
    println!("Failsafe: if it can't start it falls back to greetd's plain prompt (agreety).");
    0
}

fn disable(args: &[&str]) -> i32 {
    let mut a = vec!["disable"];
    a.extend_from_slice(args);
    if let Err(c) = need_root(&a) {
        return c;
    }
    if sys::exists(BACKUP) {
        if std::fs::copy(sys::p(BACKUP), sys::p(GREETD_CONF)).is_ok() {
            let _ = std::fs::remove_file(sys::p(BACKUP));
            println!("restored {GREETD_CONF}");
        }
    } else if read(GREETD_CONF).map_or(false, |t| t.contains("shadow-tui") || t.contains("shadow-network")) {
        let _ = write(GREETD_CONF, "[terminal]\nvt = 1\n\n[default_session]\ncommand = \"agreety --cmd /bin/sh\"\nuser = \"greeter\"\n");
        println!("reset greetd to its stock agreety config");
    }
    let prev = read(PREV_DM).unwrap_or_default().trim().to_string();
    if !prev.is_empty() {
        sh(&["systemctl", "disable", "greetd.service"]);
        let (c, o) = sh(&["systemctl", "enable", &prev]);
        println!("{}", if c == 0 { format!("re-enabled {prev}") } else { format!("could not re-enable {prev}: {o}") });
        let _ = std::fs::remove_file(sys::p(PREV_DM));
    } else {
        println!("greetd stays enabled (no previous display manager recorded); `sudo systemctl disable greetd` to turn it off");
    }
    println!("Takes effect at the next boot.");
    0
}

fn status() -> i32 {
    println!("display manager : {}", current_dm().unwrap_or_else(|| "none (TTY login)".into()));
    match read(GREETD_CONF) {
        Some(t) => println!("greetd config   : {} ({GREETD_CONF})", if t.contains("shadow-tui/lib/greeter-wrapper") { "Shadow TUI Restricted greeter" } else { "not Shadow TUI Restricted" }),
        None => println!("greetd config   : missing"),
    }
    println!("console font    : {}", font_arg(&[]));
    println!("previous DM     : {}", read(PREV_DM).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "-".into()));
    println!("{}", sh(&["shadow-greet", "--self-test"]).1);
    0
}

fn set_warning(args: &[&str]) -> i32 {
    let mut a = vec!["warning"];
    a.extend_from_slice(args);
    if let Err(c) = need_root(&a) {
        return c;
    }
    let Some(cur) = read(ND_CONF) else { return fail("login screen not configured yet: shadow greeter enable") };
    let off = args.contains(&"--off");
    let text = args.iter().filter(|a| **a != "--off").copied().collect::<Vec<_>>().join(" ").trim().to_string();
    let new = if off { "warning=\n".to_string() } else if text.is_empty() { String::new() } else { format!("warning={text}\n") };
    let kept: String = cur.split_inclusive('\n').filter(|l| !l.starts_with("warning=")).collect();
    let _ = write(ND_CONF, &(kept + &new));
    println!("{}", if off { "warning hidden".to_string() } else if text.is_empty() { "warning reset to the default notice".into() } else { format!("warning set: {text}") });
    0
}

fn preview(_args: &[&str]) -> i32 {
    use std::os::unix::process::CommandExt;
    let Some(g) = sys::which("shadow-greet") else { return fail("shadow-greet not found") };
    let e = std::process::Command::new(g).arg("--demo").exec();
    fail(&format!("shadow-greet: {e}"))
}

pub fn main(_p: &Paths, args: &[&str]) -> i32 {
    let _ = Path::new(WRAPPER);
    match args.split_first() {
        Some((&"enable", r)) => enable(r),
        Some((&"disable", r)) => disable(r),
        Some((&"status", _)) => status(),
        Some((&"preview", r)) => preview(r),
        Some((&"warning", r)) => set_warning(r),
        _ => fail(USAGE),
    }
}
