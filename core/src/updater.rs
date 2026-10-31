//! Git updates (port of updater.py): check the configured repo, build with makepkg (the package's
//! own tests run), install only if everything passes, keep the previous package for rollback.
use crate::st::{self, Paths};
use crate::sys;
use std::path::{Path, PathBuf};

/// The package was renamed in Shadow TUI Restricted; sources with the old name still build.
pub const PKGNAMES: [&str; 2] = ["shadow-tui-restricted", "shadow-network"];
// (the PKGBUILD is a split package: shadow-tui-restricted + the optional shadow-tui-sec)

fn cache() -> PathBuf {
    sys::cache_dir()
}
fn src() -> PathBuf {
    cache().join("src")
}
fn pkgs() -> PathBuf {
    cache().join("packages")
}
fn previous() -> PathBuf {
    cache().join("previous-package")
}
fn last_check(p: &Paths) -> PathBuf {
    p.state.join("last-update-check")
}

pub fn build_info(p: &Paths) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = vec![("version".into(), "?".into()), ("commit".into(), "unknown".into()), ("repo".into(), String::new()), ("branch".into(), String::new())];
    for l in std::fs::read_to_string(p.share.join("BUILD_INFO")).unwrap_or_default().lines() {
        if let Some((k, val)) = l.split_once('=') {
            match v.iter_mut().find(|(kk, _)| kk == k.trim()) {
                Some(e) => e.1 = val.trim().into(),
                None => v.push((k.trim().into(), val.trim().into())),
            }
        }
    }
    v
}

fn bi(v: &[(String, String)], k: &str) -> String {
    v.iter().find(|(kk, _)| kk == k).map(|(_, x)| x.clone()).unwrap_or_default()
}

pub fn conf(p: &Paths) -> (String, String, bool) {
    let _ = p;
    let repo = st::get("updates", "repo").unwrap_or_default().trim().to_string();
    let branch = st::get("updates", "branch").filter(|b| !b.trim().is_empty()).unwrap_or_else(|| "main".into()).trim().to_string();
    (repo, branch, st::get_bool("updates", "check_on_login", true))
}

pub fn validate(repo: &str, branch: &str) -> Option<String> {
    if repo.is_empty() {
        return Some("no repository configured (Settings → Updates)".into());
    }
    let schemes = ["https://", "http://", "ssh://", "git@", "file://", "/"];
    if !schemes.iter().any(|s| repo.starts_with(s)) || repo.contains(char::is_whitespace) || schemes.contains(&repo) {
        return Some(format!("repository must be an https://, ssh://, git@, file:// URL or a local path: '{repo}'"));
    }
    if branch.is_empty() || branch.len() > 100 || branch.contains("..") || !branch.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c)) {
        return Some(format!("invalid branch name: '{branch}'"));
    }
    if sys::which("git").is_none() {
        return Some("git is not installed".into());
    }
    None
}

pub fn remote_head(repo: &str, branch: &str) -> Result<String, String> {
    let (code, out, err) = sys::run(&["git", "ls-remote", repo, &format!("refs/heads/{branch}")]);
    if code != 0 {
        let text = if err.is_empty() { out } else { err };
        let lines: Vec<&str> = text.lines().collect();
        let fatal = lines.iter().find(|l| l.starts_with("fatal:") || l.starts_with("error:"));
        return Err(fatal.or(lines.first()).map(|s| s.to_string()).unwrap_or_else(|| "git ls-remote failed".into()));
    }
    out.lines().next().and_then(|l| l.split_whitespace().next()).map(String::from)
        .ok_or_else(|| format!("branch '{branch}' not found in {repo}"))
}

/// (status, message): up-to-date | available | error | not-configured
pub fn check(p: &Paths) -> (String, String) {
    let (repo, branch, _) = conf(p);
    let b = build_info(p);
    if let Some(e) = validate(&repo, &branch) {
        return (if repo.is_empty() { "not-configured" } else { "error" }.into(), e);
    }
    let head = match remote_head(&repo, &branch) {
        Ok(h) => h,
        Err(e) => return ("error".into(), e),
    };
    let _ = std::fs::create_dir_all(&p.state);
    let _ = std::fs::write(last_check(p), sys::now().to_string());
    let commit = bi(&b, "commit");
    let ver = bi(&b, "version");
    if commit == head {
        return ("up-to-date".into(), format!("Up to date ({ver} @ {})", &head[..8.min(head.len())]));
    }
    if commit.is_empty() || commit == "unknown" {
        return ("available".into(), format!("Installed build has no git commit recorded; {branch} is at {}", &head[..8.min(head.len())]));
    }
    ("available".into(), format!("Update available: {} → {} ({branch})", &commit[..8.min(commit.len())], &head[..8.min(head.len())]))
}

fn step(msg: &str) {
    println!("\x1b[38;2;252;238;10m▌\x1b[0m {msg}");
}

fn sh(cmd: &[&str], cwd: Option<&Path>, env: &[(&str, String)]) -> i32 {
    println!("\x1b[2m  $ {}\x1b[0m", cmd.join(" "));
    let mut c = std::process::Command::new(cmd[0]);
    c.args(&cmd[1..]);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.status().map(|s| s.code().unwrap_or(1)).unwrap_or(127)
}

fn fetch(repo: &str, branch: &str) -> bool {
    let s = src();
    let sd = s.to_string_lossy().to_string();
    if s.join(".git").is_dir() {
        let url = sys::run(&["git", "-C", &sd, "remote", "get-url", "origin"]).1;
        if url.trim() == repo && sh(&["git", "-C", &sd, "fetch", "--depth", "1", "origin", branch], None, &[]) == 0
            && sh(&["git", "-C", &sd, "reset", "--hard", "FETCH_HEAD"], None, &[]) == 0 {
            sh(&["git", "-C", &sd, "clean", "-fdx", "-q"], None, &[]);
            return true;
        }
    }
    let _ = std::fs::remove_dir_all(&s);
    let _ = std::fs::create_dir_all(cache());
    sh(&["git", "clone", "--depth", "1", "--branch", branch, repo, &sd], None, &[]) == 0
}

pub fn installed() -> Option<(String, String)> {
    for n in PKGNAMES {
        let (code, out, _) = sys::run(&["pacman", "-Q", n]);
        if code == 0 {
            return out.split_whitespace().nth(1).map(|v| (n.to_string(), v.to_string()));
        }
    }
    None
}

fn package_files() -> Vec<PathBuf> {
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(pkgs()).map(|rd| rd.flatten().filter(|e| {
        let n = e.file_name().to_string_lossy().into_owned();
        PKGNAMES.iter().any(|p| n.starts_with(&format!("{p}-"))) && n.contains(".pkg.tar.")
    }).map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), e.path())).collect()).unwrap_or_default();
    v.sort();
    v.into_iter().map(|(_, p)| p).collect()
}

pub fn build_and_install(srcdir: &Path, yes: bool, label: &str) -> i32 {
    let pkgb = std::fs::read_to_string(srcdir.join("PKGBUILD")).unwrap_or_default();
    if !PKGNAMES.iter().any(|n| pkgb.contains(&format!("pkgname={n}"))) {
        println!("update: {label} does not contain a Shadow TUI PKGBUILD — refusing to build it.");
        return 3;
    }
    step("Building (the package's own tests run during the build; nothing is installed yet)");
    let _ = std::fs::create_dir_all(pkgs());
    let before = package_files();
    let mut flags = vec!["makepkg", "-s", "-f", "-c"];
    if yes {
        flags.push("--noconfirm");
    }
    if sh(&flags, Some(srcdir), &[("PKGDEST", pkgs().to_string_lossy().into_owned())]) != 0 {
        println!("update: build or tests failed — your installed version is unchanged.");
        return 4;
    }
    let after = package_files();
    let new: Vec<&PathBuf> = after.iter().filter(|p| !before.contains(p)).collect();
    let Some(newpkg) = new.last().copied().or(after.last()) else {
        println!("update: build produced no package file — nothing changed.");
        return 4;
    };
    if let Some((name, ver)) = installed() {
        if let Some(old) = after.iter().filter(|p| p.file_name().map_or(false, |n| n.to_string_lossy().starts_with(&format!("{name}-{ver}-")))).last() {
            let _ = std::fs::write(previous(), old.to_string_lossy().as_bytes());
        }
    }
    step(&format!("Installing {}", newpkg.file_name().unwrap().to_string_lossy()));
    let np = newpkg.to_string_lossy().to_string();
    let mut cmd = vec!["sudo", "pacman", "-U"];
    if yes {
        cmd.push("--noconfirm");
    }
    cmd.push(&np);
    if sh(&cmd, None, &[]) != 0 {
        println!("update: install cancelled or failed — previous version still installed.");
        return 5;
    }
    let prev = std::fs::read_to_string(previous()).unwrap_or_default();
    let files = package_files();
    if files.len() > 4 {
        for f in &files[..files.len() - 4] {
            if f.to_string_lossy() != prev.trim() {
                let _ = std::fs::remove_file(f);
            }
        }
    }
    step("Reloading the compositor configuration (takes full effect at the next login)");
    sh(&["shadowctl", "config-reload"], None, &[]);
    step(&format!("Updated.{}", if previous().exists() { "  Roll back with: shadow update rollback" } else { "" }));
    0
}

pub fn run_update(p: &Paths, yes: bool, force: bool) -> i32 {
    let (repo, branch, _) = conf(p);
    if let Some(e) = validate(&repo, &branch) {
        println!("update: {e}");
        return 2;
    }
    step(&format!("Checking {repo} ({branch})"));
    let (st, msg) = check(p);
    println!("  {msg}");
    if st == "up-to-date" && !force {
        println!("  Nothing to do (use --force to rebuild anyway).");
        return 0;
    }
    if st == "error" {
        return 2;
    }
    step("Fetching source");
    if !fetch(&repo, &branch) {
        println!("update: could not fetch the repository — nothing changed.");
        return 3;
    }
    build_and_install(&src(), yes, &repo)
}

pub fn rollback(yes: bool) -> i32 {
    let prev = std::fs::read_to_string(previous()).unwrap_or_default();
    if prev.trim().is_empty() || !Path::new(prev.trim()).is_file() {
        println!("rollback: no previous package recorded (only updates made with `shadow update run` can be rolled back)");
        return 2;
    }
    step(&format!("Reinstalling {}", Path::new(prev.trim()).file_name().unwrap().to_string_lossy()));
    let mut cmd = vec!["sudo", "pacman", "-U"];
    if yes {
        cmd.push("--noconfirm");
    }
    cmd.push(prev.trim());
    if sh(&cmd, None, &[]) != 0 {
        return 5;
    }
    sh(&["shadowctl", "config-reload"], None, &[]);
    0
}

pub fn due_for_login_check(p: &Paths) -> bool {
    let (repo, _, on) = conf(p);
    if !on || repo.is_empty() {
        return false;
    }
    std::fs::read_to_string(last_check(p)).ok().and_then(|t| t.trim().parse::<f64>().ok())
        .map_or(true, |t| (sys::now() as f64) - t > 20.0 * 3600.0)
}

pub fn main(p: &Paths, args: &[&str]) -> i32 {
    let yes = args.contains(&"--yes") || args.contains(&"-y");
    match args {
        ["check", rest @ ..] => {
            let (st, msg) = check(p);
            println!("{msg}");
            if rest.contains(&"--notify") && st == "available" {
                st::notify("Shadow TUI update", &format!("{msg}\nSettings → Updates → Update now"), "normal");
            }
            if st == "up-to-date" || st == "available" { 0 } else { 1 }
        }
        ["run", ..] => run_update(p, yes, args.contains(&"--force")),
        // the compositor runs this at every login; it checks at most once a day
        ["login-check"] => {
            if conf(p).2 && due_for_login_check(p) {
                let (st_, msg) = check(p);
                if st_ == "available" {
                    st::notify("Shadow TUI update", &format!("{msg}\nshadow update run"), "normal");
                }
            }
            0
        }
        ["install-local", dir, ..] => build_and_install(Path::new(dir), yes, &format!("local tree {dir}")),
        ["rollback", ..] => rollback(yes),
        ["set-repo", url, rest @ ..] => {
            let branch = rest.iter().find(|x| !x.starts_with('-')).map(|b| b.to_string()).unwrap_or_else(|| conf(p).1);
            if let Some(e) = validate(url, &branch) {
                if !e.contains("git is not installed") {
                    eprintln!("update: {e}");
                    return 1;
                }
            }
            if st::set("updates", "repo", &st::q(url)).and_then(|_| st::set("updates", "branch", &st::q(&branch))).is_err() {
                eprintln!("update: could not write {}", st::config_file().display());
                return 1;
            }
            println!("update source: {url} ({branch})");
            0
        }
        ["status"] | [] => {
            let b = build_info(p);
            let (repo, branch, on) = conf(p);
            println!("installed : {} @ {}", bi(&b, "version"), &bi(&b, "commit")[..12.min(bi(&b, "commit").len())]);
            println!("repository: {}  branch: {branch}  check on login: {}", if repo.is_empty() { "(not set)".into() } else { repo }, if on { "yes" } else { "no" });
            println!("previous  : {}", std::fs::read_to_string(previous()).map(|s| s.trim().to_string()).unwrap_or_else(|_| "-".into()));
            0
        }
        _ => {
            eprintln!("usage: shadow update check [--notify] | run [--yes] [--force] | install-local DIR | rollback | status | set-repo URL [BRANCH]");
            2
        }
    }
}
