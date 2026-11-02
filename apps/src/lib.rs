//! Shared code for the Shadow TUI Restricted applications.
pub mod cfg;
pub mod craft;
pub mod desktop;
pub mod fsops;
pub mod hw;
pub mod pam;
pub mod vertex;
pub mod wall;

use std::path::{Path, PathBuf};

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/root".into()))
}

pub fn state_dir() -> PathBuf {
    shadow_ui::theme::state_dir()
}

pub fn config_path() -> PathBuf {
    state_dir().join("config.toml")
}

pub fn atomic_write(p: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("tmp~");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, p)
}

/// (exit code, stdout+stderr); 127 when the program is missing
pub fn cmd_out(args: &[&str]) -> (i32, String) {
    match std::process::Command::new(args[0]).args(&args[1..]).stdin(std::process::Stdio::null()).output() {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.code().unwrap_or(1), s)
        }
        Err(_) => (127, format!("{}: not found", args[0])),
    }
}

/// Start a program detached. User programs go to app.slice (outside Shadow's 20 % memory
/// ceiling); Shadow's own apps stay in the session slice.
pub fn spawn(cmd: &str) {
    let prog = cmd.split_whitespace().next().unwrap_or("");
    let base = prog.rsplit('/').next().unwrap_or(prog);
    let have_run = Path::new("/usr/bin/systemd-run").exists() && std::env::var_os("SHADOW_NO_SCOPES").is_none();
    let full = if base.starts_with("shadow") || !have_run {
        cmd.to_string()
    } else {
        format!("systemd-run --user --scope --slice=app.slice --collect --quiet -- sh -c {}", sh_quote(cmd))
    };
    let _ = std::process::Command::new("sh")
        .args(["-c", &format!("setsid -f sh -c {} >/dev/null 2>&1", sh_quote(&full))])
        .stdin(std::process::Stdio::null())
        .status();
}

pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn local_hour() -> usize {
    #[repr(C)]
    struct Tm {
        v: [i32; 9],
        gmtoff: i64,
        zone: *const i8,
    }
    extern "C" {
        fn time(t: *mut i64) -> i64;
        fn localtime_r(t: *const i64, tm: *mut Tm) -> *mut Tm;
    }
    // SAFETY: libc calls with valid pointers
    unsafe {
        let t = time(std::ptr::null_mut());
        let mut tm: Tm = std::mem::zeroed();
        if localtime_r(&t, &mut tm).is_null() {
            return 12;
        }
        tm.v[2].clamp(0, 23) as usize
    }
}

/// app id of the window niri has focused right now ("" if none / not in niri)
pub fn focused_app() -> String {
    shadow_niri::json(&["focused-window"]).ok().and_then(|v| v.get("app_id").and_then(|a| a.as_str()).map(|s| s.to_lowercase())).unwrap_or_default()
}

/// Known terminals: (command, how it takes a program to run). Order = the order offered.
pub const TERMINALS: &[(&str, &str, &str)] = &[
    // (command, name, flag before the program; "" = the program follows directly)
    ("shadow-term", "Shadow terminal", "-e"),
    ("foot", "Foot", "-e"),
    ("kitty", "Kitty", ""),
    ("alacritty", "Alacritty", "-e"),
    ("wezterm", "WezTerm", "start --"),
    ("ptyxis", "Ptyxis", "--"),
    ("gnome-terminal", "GNOME Terminal", "--"),
    ("konsole", "Konsole", "-e"),
    ("xterm", "XTerm", "-e"),
];

/// The default terminal command ([defaults] terminal_cmd), "shadow-term" when unset.
pub fn terminal() -> String {
    cfg::get_in(&std::fs::read_to_string(config_path()).unwrap_or_default(), "defaults", "terminal_cmd")
        .map(|v| v.trim_matches('"').trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "shadow-term".into())
}

/// A command line that runs `cmd` (already shell-ready) in the default terminal.
pub fn terminal_exec(cmd: &str) -> String {
    terminal_exec_with(&terminal(), cmd)
}

pub fn terminal_exec_with(term: &str, cmd: &str) -> String {
    let prog = term.split_whitespace().next().unwrap_or("shadow-term");
    let flag = TERMINALS.iter().find(|t| t.0 == prog).map(|t| t.2).unwrap_or("-e");
    if flag.is_empty() {
        format!("{term} {cmd}")
    } else {
        format!("{term} {flag} {cmd}")
    }
}

/// Open the default terminal in `dir`.
pub fn terminal_here(dir: &str) -> String {
    format!("cd {} && exec {}", sh_quote(dir), terminal())
}

#[cfg(test)]
mod term_tests {
    use super::*;
    #[test]
    fn terminal_flags() {
        assert_eq!(terminal_exec_with("foot", "htop"), "foot -e htop");
        assert_eq!(terminal_exec_with("kitty", "htop"), "kitty htop");
        assert_eq!(terminal_exec_with("wezterm", "htop"), "wezterm start -- htop");
        assert_eq!(terminal_exec_with("myterm", "htop"), "myterm -e htop");
    }
}
