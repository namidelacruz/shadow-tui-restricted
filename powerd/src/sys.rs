//! System access. Every path goes through `p()` so tests can point the whole tool at a fake
//! root with SHADOW_OPT_ROOT (fake /sys, /proc, /etc, /run, /usr/lib/firmware ...).
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn root() -> PathBuf {
    std::env::var_os("SHADOW_OPT_ROOT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn p(rel: &str) -> PathBuf {
    root().join(rel.trim_start_matches('/'))
}

pub fn read(rel: &str) -> Option<String> {
    read_path(&p(rel))
}

pub fn read_path(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

pub fn write(rel: &str, val: &str) -> Result<(), String> {
    let path = p(rel);
    std::fs::write(&path, val).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn exists(rel: &str) -> bool {
    p(rel).exists()
}

/// Sorted entry names of a directory ("" list if missing).
pub fn ls(rel: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(p(rel))
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Basename of a symlink target (e.g. a device's driver).
pub fn link_name(rel: &str) -> Option<String> {
    std::fs::read_link(p(rel)).ok().and_then(|t| t.file_name().map(|n| n.to_string_lossy().into_owned()))
}

pub fn which(cmd: &str) -> Option<PathBuf> {
    if cmd.contains('/') {
        let pb = PathBuf::from(cmd);
        return pb.exists().then_some(pb);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(cmd)).find(|c| c.is_file())
}

/// Run a command; (exit code, stdout, stderr). 127 = not found.
pub fn run(args: &[&str]) -> (i32, String, String) {
    run_input(args, None)
}

pub fn run_input(args: &[&str], input: Option<&str>) -> (i32, String, String) {
    let Some((cmd, rest)) = args.split_first() else { return (127, String::new(), String::new()) };
    let mut c = Command::new(cmd);
    c.args(rest).env("LC_ALL", "C").stdout(Stdio::piped()).stderr(Stdio::piped());
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let Ok(mut child) = c.spawn() else { return (127, String::new(), format!("{cmd}: not found")) };
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        use std::io::Write;
        let _ = stdin.write_all(text.as_bytes());
    }
    match child.wait_with_output() {
        Ok(o) => (
            o.status.code().unwrap_or(1),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ),
        Err(e) => (1, String::new(), e.to_string()),
    }
}

pub fn is_root() -> bool {
    // no libc crate: the effective uid is in /proc/self/status ("Uid: real effective ...")
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| s.lines().find(|l| l.starts_with("Uid:")).map(|l| l.split_whitespace().nth(2) == Some("0")))
        .unwrap_or(false)
}

pub fn svc_active(unit: &str) -> bool {
    run(&["systemctl", "is-active", "-q", unit]).0 == 0
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/state")).join("shadow-tui")
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config")).join("shadow-state")
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("shadow-tui")
}

pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".cache")).join("shadow-tui")
}

pub fn run_dir() -> PathBuf {
    p("run/shadow-powerd")
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Local hour of day (0-23) from /etc/localtime offset is overkill; `date +%H` respects TZ.
pub fn hour() -> u32 {
    run(&["date", "+%H"]).1.parse().unwrap_or(12)
}

pub fn log_line(path: &Path, msg: &str) {
    use std::io::Write;
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {msg}", run(&["date", "+%F %T"]).1);
    }
}

pub fn atomic_write(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

/// key = value files (comments with #)
pub fn read_kv(path: &Path) -> Vec<(String, String)> {
    read_path(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().trim_matches('"').to_string()))
        })
        .collect()
}

pub fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}
