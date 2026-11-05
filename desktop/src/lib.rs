//! Shared pieces of the session services.
#[path = "sys.rs"]
pub mod sys;
pub mod luahost;
pub mod privilege;
pub mod telemetry;

use std::path::PathBuf;

pub fn runtime_dir() -> PathBuf {
    let d = PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join("shadow");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Start a program the way the session does: user programs go to app.slice (the 80 % of RAM
/// outside Shadow's cap); Shadow's own apps stay in the session's shadow.slice.
pub fn spawn(cmd: &str) {
    let prog = cmd.split_whitespace().next().unwrap_or("");
    let base = prog.rsplit('/').next().unwrap_or(prog);
    let shadow_app = base.starts_with("shadow-") || base == "shadow";
    let have_run = std::path::Path::new("/usr/bin/systemd-run").exists() && std::env::var_os("SHADOW_NO_SCOPES").is_none();
    let full = if shadow_app || !have_run {
        cmd.to_string()
    } else {
        format!("systemd-run --user --scope --slice=app.slice --collect --quiet -- sh -c '{}'", cmd.replace('\'', "'\\''"))
    };
    let _ = std::process::Command::new("sh").args(["-c", &format!("{full} >/dev/null 2>&1 &")]).status();
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn log(tag: &str, msg: &str) {
    eprintln!("{tag}: {msg}");
}
