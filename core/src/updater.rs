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
    let mut v: Vec<(String, String)> = vec![
        ("version".into(), "?".into()),
        ("commit".into(), "unknown".into()),
        ("repo".into(), String::new()),
        ("branch".into(), String::new()),
    ];
    for l in std::fs::read_to_string(p.share.join("BUILD_INFO"))
        .unwrap_or_default()
        .lines()
    {
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
    v.iter()
        .find(|(kk, _)| kk == k)
        .map(|(_, x)| x.clone())
        .unwrap_or_default()
}

pub fn conf(p: &Paths) -> (String, String, bool) {
    let _ = p;
    let repo = st::get("updates", "repo")
        .unwrap_or_default()
        .trim()
        .to_string();
    let branch = st::get("updates", "branch")
        .filter(|b| !b.trim().is_empty())
        .unwrap_or_else(|| "main".into())
        .trim()
        .to_string();
    (repo, branch, false)
}

pub fn validate(repo: &str, branch: &str) -> Option<String> {
    if repo.is_empty() {
        return Some("no repository configured (Settings → Updates)".into());
    }
    let schemes = ["https://", "http://", "ssh://", "git@", "file://", "/"];
    if !schemes.iter().any(|s| repo.starts_with(s))
        || repo.contains(char::is_whitespace)
        || schemes.contains(&repo)
    {
        return Some(format!(
            "repository must be an https://, ssh://, git@, file:// URL or a local path: '{repo}'"
        ));
    }
    if branch.is_empty()
        || branch.len() > 100
        || branch.contains("..")
        || !branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
    {
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
        let fatal = lines
            .iter()
            .find(|l| l.starts_with("fatal:") || l.starts_with("error:"));
        return Err(fatal
            .or(lines.first())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "git ls-remote failed".into()));
    }
    out.lines()
        .next()
        .and_then(|l| l.split_whitespace().next())
        .map(String::from)
        .ok_or_else(|| format!("branch '{branch}' not found in {repo}"))
}

/// (status, message): up-to-date | available | error | not-configured
pub fn check(p: &Paths) -> (String, String) {
    let (repo, branch, _) = conf(p);
    let b = build_info(p);
    if let Some(e) = validate(&repo, &branch) {
        return (
            if repo.is_empty() {
                "not-configured"
            } else {
                "error"
            }
            .into(),
            e,
        );
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
        return (
            "up-to-date".into(),
            format!("Up to date ({ver} @ {})", &head[..8.min(head.len())]),
        );
    }
    if commit.is_empty() || commit == "unknown" {
        return (
            "available".into(),
            format!(
                "Installed build has no git commit recorded; {branch} is at {}",
                &head[..8.min(head.len())]
            ),
        );
    }
    (
        "available".into(),
        format!(
            "Update available: {} → {} ({branch})",
            &commit[..8.min(commit.len())],
            &head[..8.min(head.len())]
        ),
    )
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
        if url.trim() == repo
            && sh(
                &["git", "-C", &sd, "fetch", "--depth", "1", "origin", branch],
                None,
                &[],
            ) == 0
            && sh(
                &["git", "-C", &sd, "reset", "--hard", "FETCH_HEAD"],
                None,
                &[],
            ) == 0
        {
            sh(&["git", "-C", &sd, "clean", "-fdx", "-q"], None, &[]);
            return true;
        }
    }
    let _ = std::fs::remove_dir_all(&s);
    let _ = std::fs::create_dir_all(cache());
    sh(
        &[
            "git", "clone", "--depth", "1", "--branch", branch, repo, &sd,
        ],
        None,
        &[],
    ) == 0
}

pub fn installed() -> Option<(String, String)> {
    crate::distro::installed_version(&PKGNAMES)
}

fn rpm() -> bool {
    crate::distro::family() == crate::distro::Family::Rpm
}

fn package_files() -> Vec<PathBuf> {
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(pkgs())
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    PKGNAMES.iter().any(|p| n.starts_with(&format!("{p}-")))
                        && (n.contains(".pkg.tar.")
                            || (n.ends_with(".rpm") && !n.ends_with(".src.rpm")))
                        && !n.starts_with("shadow-tui-restricted-debug")
                })
                .map(|e| {
                    (
                        e.metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::UNIX_EPOCH),
                        e.path(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.into_iter().map(|(_, p)| p).collect()
}

pub fn build_and_install(srcdir: &Path, yes: bool, label: &str) -> i32 {
    let recipe = if rpm() {
        srcdir.join("packaging/shadow-tui-restricted.spec")
    } else {
        srcdir.join("PKGBUILD")
    };
    let pkgb = std::fs::read_to_string(&recipe).unwrap_or_default();
    if !PKGNAMES.iter().any(|n| {
        pkgb.contains(&format!("pkgname={n}")) || pkgb.contains(&format!("Name:           {n}"))
    }) {
        println!("update: {label} does not contain a Shadow TUI build recipe ({}) — refusing to build it.", recipe.file_name().unwrap().to_string_lossy());
        return 3;
    }
    step("Building (the package's own tests run during the build; nothing is installed yet)");
    let _ = std::fs::create_dir_all(pkgs());
    let before = package_files();
    let out = pkgs().to_string_lossy().into_owned();
    let built = if rpm() {
        sh(&["packaging/build-rpm.sh", &out], Some(srcdir), &[])
    } else {
        let mut flags = vec!["makepkg", "-s", "-f", "-c"];
        if yes {
            flags.push("--noconfirm");
        }
        sh(&flags, Some(srcdir), &[("PKGDEST", out.clone())])
    };
    if built != 0 {
        println!("update: build or tests failed — your installed version is unchanged.");
        return 4;
    }
    let after: Vec<PathBuf> = package_files()
        .into_iter()
        .filter(|p| {
            p.file_name().map_or(false, |n| {
                n.to_string_lossy().starts_with("shadow-tui-restricted-")
            })
        })
        .collect();
    let new: Vec<&PathBuf> = after.iter().filter(|p| !before.contains(p)).collect();
    let Some(newpkg) = new.last().copied().or(after.last()) else {
        println!("update: build produced no package file — nothing changed.");
        return 4;
    };
    if let Some((name, ver)) = installed() {
        if let Some(old) = after
            .iter()
            .filter(|p| {
                p.file_name().map_or(false, |n| {
                    n.to_string_lossy().starts_with(&format!("{name}-{ver}-"))
                })
            })
            .last()
        {
            let _ = std::fs::write(previous(), old.to_string_lossy().as_bytes());
        }
    }
    step(&format!(
        "Installing {}",
        newpkg.file_name().unwrap().to_string_lossy()
    ));
    let np = newpkg.to_string_lossy().to_string();
    let mut cmd = if rpm() {
        vec!["sudo", "dnf", "install"]
    } else {
        vec!["sudo", "pacman", "-U"]
    };
    if yes {
        cmd.push(if rpm() { "-y" } else { "--noconfirm" });
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
    step("Regenerating the niri configuration and reloading the Lua modules");
    sh(&["shadow", "setup", "niri"], None, &[]);
    sh(&["shadow-sessiond", "reload"], None, &[]);
    step(&format!(
        "Updated.{}",
        if previous().exists() {
            "  Roll back with: shadow update rollback"
        } else {
            ""
        }
    ));
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
    step(&format!(
        "Reinstalling {}",
        Path::new(prev.trim())
            .file_name()
            .unwrap()
            .to_string_lossy()
    ));
    let mut cmd = if rpm() {
        vec!["sudo", "rpm", "-Uvh", "--oldpackage"]
    } else {
        vec!["sudo", "pacman", "-U"]
    };
    if yes && !rpm() {
        cmd.push("--noconfirm");
    }
    cmd.push(prev.trim());
    if sh(&cmd, None, &[]) != 0 {
        return 5;
    }
    sh(&["shadow", "setup", "niri"], None, &[]);
    sh(&["shadow-sessiond", "reload"], None, &[]);
    0
}

pub fn main(p: &Paths, args: &[&str]) -> i32 {
    let yes = args.contains(&"--yes") || args.contains(&"-y");
    match args {
        ["check", rest @ ..] => {
            let (st, msg) = check(p);
            println!("{msg}");
            if rest.contains(&"--notify") && st == "available" {
                st::notify(
                    "Shadow TUI update",
                    &format!("{msg}\nSettings → Updates → Update now"),
                    "normal",
                );
            }
            if st == "up-to-date" || st == "available" {
                0
            } else {
                1
            }
        }
        ["run", ..] => run_update(p, yes, args.contains(&"--force")),
        // checking is on demand only (Update app → "Check for updates"); kept so old sessions that
        // still call it at login don't error
        ["login-check"] => 0,
        ["check-files", rest @ ..] => check_files(p, rest.contains(&"--porcelain")),
        ["apply", rest @ ..] => apply(
            p,
            rest.contains(&"--porcelain"),
            rest.contains(&"--password-stdin"),
        ),
        ["install-staged", stage, plan] => {
            install_staged(Path::new(stage), Path::new(plan), &sysroot())
        }
        ["undo"] => undo(&sysroot()),
        ["install-local", dir, ..] => {
            build_and_install(Path::new(dir), yes, &format!("local tree {dir}"))
        }
        ["rollback", ..] => rollback(yes),
        ["set-repo", url, rest @ ..] => {
            let branch = rest
                .iter()
                .find(|x| !x.starts_with('-'))
                .map(|b| b.to_string())
                .unwrap_or_else(|| conf(p).1);
            if let Some(e) = validate(url, &branch) {
                if !e.contains("git is not installed") {
                    eprintln!("update: {e}");
                    return 1;
                }
            }
            if st::set("updates", "repo", &st::q(url))
                .and_then(|_| st::set("updates", "branch", &st::q(&branch)))
                .is_err()
            {
                eprintln!("update: could not write {}", st::config_file().display());
                return 1;
            }
            println!("update source: {url} ({branch})");
            0
        }
        ["status"] | [] => {
            let b = build_info(p);
            let (repo, branch, on) = conf(p);
            println!(
                "installed : {} @ {}",
                bi(&b, "version"),
                &bi(&b, "commit")[..12.min(bi(&b, "commit").len())]
            );
            let _ = on;
            println!(
                "repository: {}  branch: {branch}  (checked only when you ask)",
                if repo.is_empty() {
                    "(not set)".into()
                } else {
                    repo
                }
            );
            println!(
                "previous  : {}",
                std::fs::read_to_string(previous())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "-".into())
            );
            0
        }
        _ => {
            eprintln!("usage: shadow update check | check-files [--porcelain] | apply [--porcelain] [--password-stdin] | undo\n                     | run [--yes] [--force] (full package rebuild) | install-local DIR | rollback | status | set-repo URL [BRANCH]");
            2
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════════════════
//  File-level updates: `shadow update check --files` / `apply` / `undo`
//
//  On demand only. The source is fetched, rebuilt incrementally (cargo recompiles only the crates
//  a change touches), laid out in a staging tree with the same install-files.sh the packages use,
//  and compared byte for byte with the installed system. Only files that differ are replaced —
//  atomically (new file + rename, so running programs keep their old copy until restarted) and
//  with a backup of every file touched, so `shadow update undo` restores the previous state.
//  Changed config files in /etc are never overwritten: the new version lands next to yours as
//  FILE.shadow-new. Package database note: pacman/rpm still list the old package version until
//  the next full `./install.sh`, which re-syncs everything.
// ════════════════════════════════════════════════════════════════════════════════════════

const BACKUPS: &str = "/var/lib/shadow-tui/update-backup";
/// The config defaults as last shipped (laid out by install-files.sh): a changed /etc file is only
/// offered (as .shadow-new) when the shipped default itself changed, not because you edited yours.
const SHIPPED: &str = "/opt/shadow-tui/share/etc-defaults";
const META: [&str; 4] = [
    "/opt/shadow-tui/share/BUILD_INFO",
    "/opt/shadow-tui/share/MANIFEST.sha512",
    "/opt/shadow-tui/share/FILES",
    "/opt/shadow-tui/share/FILES.sec",
];

/// The system root: `/`, or a scratch tree for end-to-end tests (`SHADOW_UPDATE_TEST_ROOT=/abs/dir`;
/// sudo's env_reset drops it, so it can't redirect a real administrator step).
fn sysroot() -> PathBuf {
    match std::env::var("SHADOW_UPDATE_TEST_ROOT") {
        Ok(r) if r.starts_with('/') && Path::new(&r).is_dir() => PathBuf::from(r),
        _ => PathBuf::from("/"),
    }
}

fn stage_dir() -> PathBuf {
    cache().join("stage")
}
fn target_dir() -> PathBuf {
    cache().join("target")
}

fn out(porcelain: bool, kind: &str, msg: &str) {
    if porcelain {
        println!("{kind} {msg}");
    } else {
        match kind {
            "step" => step(msg),
            "result" => println!("{msg}"),
            _ => println!("  {msg}"),
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> (i32, String) {
    let d = dir.to_string_lossy().to_string();
    let mut a = vec!["git", "-C", d.as_str()];
    a.extend_from_slice(args);
    let (c, o, e) = sys::run(&a);
    (c, if c == 0 { o } else { e })
}

/// Bring the cached source to the branch head with full history (needed to diff against the
/// installed commit). Returns the head commit.
fn sync_source(repo: &str, branch: &str) -> Result<String, String> {
    let s = src();
    let fresh =
        !s.join(".git").is_dir() || git(&s, &["remote", "get-url", "origin"]).1.trim() != repo;
    if fresh {
        let _ = std::fs::remove_dir_all(&s);
        let _ = std::fs::create_dir_all(cache());
        let (c, _, e) = sys::run(&[
            "git",
            "clone",
            "-q",
            "--branch",
            branch,
            repo,
            &s.to_string_lossy(),
        ]);
        if c != 0 {
            return Err(format!("git clone failed: {}", e.trim()));
        }
    } else {
        let shallow = s.join(".git/shallow").exists();
        let mut a = vec!["fetch", "-q", "origin", branch];
        if shallow {
            a.insert(1, "--unshallow");
        }
        let (c, e) = git(&s, &a);
        if c != 0 {
            return Err(format!("git fetch failed: {}", e.trim()));
        }
        let (c, e) = git(&s, &["reset", "-q", "--hard", "FETCH_HEAD"]);
        if c != 0 {
            return Err(format!("git reset failed: {}", e.trim()));
        }
        git(&s, &["clean", "-q", "-fdx", "-e", "target"]);
    }
    let (c, h) = git(&s, &["rev-parse", "HEAD"]);
    if c != 0 {
        return Err("cannot read the source commit".into());
    }
    Ok(h.trim().to_string())
}

fn known_commit(c: &str) -> bool {
    !c.is_empty()
        && c != "unknown"
        && git(&src(), &["cat-file", "-e", &format!("{c}^{{commit}}")]).0 == 0
}

/// What a changed source path affects (for the summary; the byte diff decides what is replaced)
fn component(path: &str) -> &'static str {
    let top = path.split('/').next().unwrap_or("");
    match top {
        "ui" => "every 2D app (shared toolkit)",
        "niri" => "niri integration (bar, apps, session services, config generator)",
        "bar" => "shadow-bar",
        "apps" => "shadow-launch / -net / -config / -files / -lock / -update",
        "desktop" => "shadow-sessiond / shadow-displayd",
        "greet" => "shadow-greet (login screen)",
        "term" => "shadow-term",
        "core" if path.ends_with("distro.rs") || path.ends_with("json.rs") => {
            "shadow, shadow-powerd, shadow-sec"
        }
        "core" => "shadow (sync, healer, Wi-Fi, updater)",
        "powerd" => "shadow-powerd / shadow-audiod",
        "lua" => "Lua sandbox (shadow, shadow-sessiond)",
        "sec" => "shadow-sec (optional)",
        "system" => "system files (units, session, scripts)",
        "modules" => "default Lua modules (your live modules are not replaced)",
        "share" => "shared data",
        "packaging" => "packaging",
        "docs" | "README.md" | "LICENSE" => "documentation",
        "Cargo.lock" | "Cargo.toml" => {
            "crate dependencies (all programs rebuilt; only changed binaries replaced)"
        }
        "install.sh" | "PKGBUILD" | "tests" | ".gitignore" | "shadow-tui-restricted.install" => {
            "installer / packaging only (nothing installed changes)"
        }
        _ => "other",
    }
}

/// Every dependency the packaging declares: PKGBUILD arrays (multi-line too) and spec tags.
fn deps_of(pkgbuild: &str, spec: &str) -> Vec<String> {
    let mut v = vec![];
    let mut open = false;
    for l in pkgbuild.lines() {
        let t = l.trim();
        let t = t.split(" #").next().unwrap_or(t);
        let body = if open {
            Some(t)
        } else {
            ["depends=(", "makedepends=(", "optdepends=(", "checkdepends=("].iter().find_map(|k| t.strip_prefix(k))
        };
        if let Some(b) = body {
            open = !b.contains(')');
            v.extend(b.split(')').next().unwrap_or("").split_whitespace().map(|x| x.trim_matches(['\'', '"']).to_string()));
        }
    }
    for l in spec.lines() {
        let t = l.trim();
        if ["Requires", "BuildRequires", "Recommends", "Requires(post)", "Requires(pre)"].iter().any(|k| t.starts_with(&format!("{k}:"))) {
            v.push(t.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }
    v.sort();
    v
}

/// A dependency change needs the package manager: a file-level update can't add packages.
fn reinstall_reason(base: &str, head: &str) -> Option<String> {
    let show = |c: &str, f: &str| git(&src(), &["show", &format!("{c}:{f}")]).1;
    let spec = "packaging/shadow-tui-restricted.spec";
    let (a, b) = (deps_of(&show(base, "PKGBUILD"), &show(base, spec)), deps_of(&show(head, "PKGBUILD"), &show(head, spec)));
    if a == b {
        return None;
    }
    let added: Vec<&String> = b.iter().filter(|d| !a.contains(d)).collect();
    let removed: Vec<&String> = a.iter().filter(|d| !b.contains(d)).collect();
    let mut m = String::from("the package dependencies changed");
    if !added.is_empty() {
        m += &format!(" (+ {})", added.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    }
    if !removed.is_empty() {
        m += &format!(" (- {})", removed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    }
    Some(m)
}

pub fn check_files(p: &Paths, porcelain: bool) -> i32 {
    let (repo, branch, _) = conf(p);
    if let Some(e) = validate(&repo, &branch) {
        out(
            porcelain,
            "status",
            if repo.is_empty() {
                "not-configured"
            } else {
                "error"
            },
        );
        out(porcelain, "message", &e);
        return 1;
    }
    let b = build_info(p);
    let base = bi(&b, "commit");
    out(
        porcelain,
        "installed",
        &format!("{} {}", bi(&b, "version"), base),
    );
    let head = match sync_source(&repo, &branch) {
        Ok(h) => h,
        Err(e) => {
            out(porcelain, "status", "error");
            out(porcelain, "message", &e);
            return 1;
        }
    };
    out(porcelain, "latest", &head);
    let _ = std::fs::create_dir_all(&p.state);
    let _ = std::fs::write(last_check(p), sys::now().to_string());
    if head == base {
        out(porcelain, "status", "up-to-date");
        out(
            porcelain,
            "message",
            &format!(
                "Up to date ({} @ {})",
                bi(&b, "version"),
                &head[..8.min(head.len())]
            ),
        );
        return 0;
    }
    out(porcelain, "status", "available");
    if !known_commit(&base) {
        out(
            porcelain,
            "message",
            "the installed build's commit is not in this repository: every file will be compared",
        );
        return 0;
    }
    let n = git(&src(), &["rev-list", "--count", &format!("{base}..{head}")])
        .1
        .trim()
        .to_string();
    out(
        porcelain,
        "message",
        &format!(
            "{n} new commit(s) on {branch}: {} → {}",
            &base[..8],
            &head[..8]
        ),
    );
    for l in git(
        &src(),
        &[
            "log",
            "--format=%h %s",
            "-n",
            "40",
            &format!("{base}..{head}"),
        ],
    )
    .1
    .lines()
    {
        out(porcelain, "commit", l);
    }
    let files = git(&src(), &["diff", "--name-only", &base, &head]).1;
    let mut comps = std::collections::BTreeSet::new();
    for f in files.lines().filter(|l| !l.is_empty()) {
        out(porcelain, "changed", f);
        comps.insert(component(f));
    }
    for c in comps {
        out(porcelain, "component", c);
    }
    if let Some(r) = reinstall_reason(&base, &head) {
        out(porcelain, "reinstall", &r);
    }
    0
}

fn cargo() -> Option<String> {
    sys::which("cargo")
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| {
            let p = st::home().join(".cargo/bin/cargo");
            p.is_file().then(|| p.to_string_lossy().into_owned())
        })
}

/// Run a command, streaming its output (stdout and stderr) line by line as `log …`.
fn stream(porcelain: bool, cmd: &[&str], cwd: &Path, env: &[(&str, String)]) -> i32 {
    use std::io::{BufRead, BufReader};
    let mut c = std::process::Command::new(cmd[0]);
    c.args(&cmd[1..])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in env {
        c.env(k, v);
    }
    let Ok(mut child) = c.spawn() else {
        out(porcelain, "log", &format!("{}: not found", cmd[0]));
        return 127;
    };
    let err = child.stderr.take();
    let t = std::thread::spawn(move || {
        if let Some(e) = err {
            for l in BufReader::new(e).lines().map_while(Result::ok) {
                out(porcelain, "log", &l);
            }
        }
    });
    if let Some(o) = child.stdout.take() {
        for l in BufReader::new(o).lines().map_while(Result::ok) {
            out(porcelain, "log", &l);
        }
    }
    let _ = t.join();
    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
}

fn read_list(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(String::from)
        .collect()
}

fn same_entry(a: &Path, b: &Path) -> bool {
    match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
        (Ok(ma), Ok(mb)) if ma.file_type().is_symlink() && mb.file_type().is_symlink() => {
            std::fs::read_link(a).ok() == std::fs::read_link(b).ok()
        }
        (Ok(ma), Ok(mb)) if ma.is_file() && mb.is_file() => {
            use std::os::unix::fs::PermissionsExt;
            ma.len() == mb.len()
                && (ma.permissions().mode() & 0o777) == (mb.permissions().mode() & 0o777)
                && std::fs::read(a).ok() == std::fs::read(b).ok()
        }
        _ => false,
    }
}

/// The plan: what differs between the staged tree and the live system.
pub fn plan(stage: &Path, live_root: &Path) -> Vec<(String, String)> {
    let share = "opt/shadow-tui/share";
    let mut files = read_list(&stage.join(share).join("FILES"));
    let sec_live = live_root.join("opt/shadow-tui/bin/shadow-sec").exists();
    if sec_live {
        files.extend(read_list(&stage.join(share).join("FILES.sec")));
    }
    let mut v = vec![];
    for f in &files {
        if META.contains(&f.as_str()) {
            continue;
        }
        let rel = f.trim_start_matches('/');
        let (s, l) = (stage.join(rel), live_root.join(rel));
        if std::fs::symlink_metadata(&l).is_err() {
            v.push(("new".into(), f.clone()));
        } else if f.starts_with("/etc/") {
            // offered as .shadow-new only when the shipped default changed (not because you edited
            // yours) and it hasn't been offered already
            let shipped = live_root.join(SHIPPED.trim_start_matches('/')).join(rel.trim_start_matches("etc/"));
            let base = if std::fs::symlink_metadata(&shipped).is_ok() { shipped } else { l.clone() };
            let pending = live_root.join(format!("{rel}.shadow-new"));
            if !same_entry(&s, &base) && !same_entry(&s, &pending) && !same_entry(&s, &l) {
                v.push(("config".into(), f.clone()));
            }
        } else if !same_entry(&s, &l) {
            v.push(("replace".into(), f.clone()));
        }
    }
    let mut old = read_list(&live_root.join(share).join("FILES"));
    if sec_live {
        old.extend(read_list(&live_root.join(share).join("FILES.sec")));
    }
    for f in old {
        if !files.contains(&f)
            && !f.starts_with("/etc/")
            && !META.contains(&f.as_str())
            && std::fs::symlink_metadata(live_root.join(f.trim_start_matches('/'))).is_ok()
        {
            v.push(("remove".into(), f));
        }
    }
    v
}

pub fn apply(p: &Paths, porcelain: bool, password_stdin: bool) -> i32 {
    let password = if password_stdin {
        let mut l = String::new();
        let _ = std::io::stdin().read_line(&mut l);
        Some(l.trim_end_matches(['\n', '\r']).to_string())
    } else {
        None
    };
    let fail = |m: &str| {
        out(porcelain, "result", &format!("fail {m}"));
        1
    };
    let (repo, branch, _) = conf(p);
    if let Some(e) = validate(&repo, &branch) {
        return fail(&e);
    }
    let Some(cargo) = cargo() else {
        return fail("cargo (Rust) is not installed — run ./install.sh once");
    };
    let b = build_info(p);
    let base = bi(&b, "commit");
    out(porcelain, "step", "Fetching the source");
    let head = match sync_source(&repo, &branch) {
        Ok(h) => h,
        Err(e) => return fail(&e),
    };
    if known_commit(&base) {
        if let Some(r) = reinstall_reason(&base, &head) {
            return fail(&format!(
                "{r}: this update needs the package manager — run ./install.sh in {}",
                src().display()
            ));
        }
    }
    let s = src();
    let tgt = target_dir();
    let env = [("CARGO_TARGET_DIR", tgt.to_string_lossy().into_owned())];
    out(
        porcelain,
        "step",
        "Building (incremental: only crates touched by the changes recompile)",
    );
    if stream(
        porcelain,
        &[&cargo, "build", "--release", "--locked", "--workspace"],
        &s,
        &env,
    ) != 0
    {
        return fail("the build failed — nothing was installed");
    }
    // tests of the crates the changes touch (all of them when dependencies changed)
    let changed = if known_commit(&base) {
        git(&s, &["diff", "--name-only", &base, &head]).1
    } else {
        "Cargo.lock\n".into()
    };
    let crates: std::collections::BTreeSet<&str> = changed
        .lines()
        .filter_map(|f| match f.split('/').next().unwrap_or("") {
            "ui" => Some("shadow-ui"),
            "niri" => Some("shadow-niri"),
            "bar" => Some("shadow-bar"),
            "apps" => Some("shadow-apps"),
            "desktop" => Some("shadow-desktop"),
            "greet" => Some("shadow-greet"),
            "term" => Some("shadow-term"),
            "core" => Some("shadow"),
            "powerd" => Some("shadow-powerd"),
            "lua" => Some("shadow-lua"),
            "sec" => Some("shadow-sec"),
            "Cargo.lock" | "Cargo.toml" => Some("*"),
            _ => None,
        })
        .collect();
    if !crates.is_empty() {
        out(porcelain, "step", "Testing what changed");
        let mut a: Vec<&str> = vec![&cargo, "test", "--release", "--locked"];
        if crates.contains("*") {
            a.push("--workspace");
        } else {
            for c in &crates {
                a.push("-p");
                a.push(c);
            }
        }
        if stream(porcelain, &a, &s, &env) != 0 {
            return fail("tests failed — nothing was installed");
        }
    }
    out(porcelain, "step", "Comparing with the installed files");
    let stage = stage_dir();
    let _ = std::fs::remove_dir_all(&stage);
    let fam = if crate::distro::family() == crate::distro::Family::Rpm {
        "rpm"
    } else {
        "arch"
    };
    let ver = format!(
        "{}+git{}",
        bi(&b, "version").split('+').next().unwrap_or("?"),
        &head[..8.min(head.len())]
    );
    let lay = |part: &str| {
        run_env(
            &[
                "sh",
                "packaging/install-files.sh",
                &stage.to_string_lossy(),
                fam,
                &tgt.to_string_lossy(),
                &s.to_string_lossy(),
                part,
            ],
            &s,
            &[("SHADOW_VERSION", ver.clone())],
        )
    };
    if lay("main") != 0 || lay("sec") != 0 {
        return fail("could not lay out the new files");
    }
    let root = sysroot();
    let pl = plan(&stage, &root);
    for (k, f) in &pl {
        out(porcelain, k, f);
    }
    let n = pl.iter().filter(|(k, _)| k != "config").count();
    if pl.is_empty() && head == base {
        out(porcelain, "result", &format!("ok already up to date ({})", &head[..8.min(head.len())]));
        return 0;
    }
    if pl.is_empty() {
        out(
            porcelain,
            "log",
            "no installed file differs; only the version record is updated",
        );
    }
    let planf = cache().join("update.plan");
    let text: String = pl.iter().map(|(k, f)| format!("{k} {f}\n")).collect();
    if std::fs::write(&planf, text).is_err() {
        return fail("cannot write the update plan");
    }
    out(
        porcelain,
        "step",
        &format!("Installing {n} changed file(s) (administrator rights)"),
    );
    if sys::is_root() {
        // already administrator (e.g. a root shell): no sudo round trip
        if install_staged(&stage, &planf, &root) != 0 {
            return fail("installing failed — see above");
        }
    } else {
        let me = if Path::new("/opt/shadow-tui/bin/shadow").exists() {
            "/opt/shadow-tui/bin/shadow".to_string()
        } else {
            std::env::current_exe()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "shadow".into())
        };
        let sp = stage.to_string_lossy().into_owned();
        let pf = planf.to_string_lossy().into_owned();
        let mut cmd = vec!["sudo"];
        if password.is_some() {
            cmd.extend(["-S", "-p", ""]);
        } else if sys::run(&["sudo", "-n", "true"]).0 != 0 && !stdin_is_tty() {
            return fail("administrator password needed");
        }
        cmd.extend([me.as_str(), "update", "install-staged", &sp, &pf]);
        let mut password = password;
        let rc = run_input_stream(&cmd, password.as_deref().map(|p| format!("{p}\n")), |l| {
            out(porcelain, "log", l)
        });
        let had_password = password.is_some();
        if let Some(p) = password.as_mut() {
            zero(p);
        }
        if rc != 0 {
            return fail(if had_password {
                "administrator step failed (wrong password?) — nothing was changed"
            } else {
                "administrator step failed — nothing was changed"
            });
        }
    }
    // session side: niri config, Lua modules, restart the session programs that changed
    sh_quiet(&["shadow", "setup", "niri"]);
    sh_quiet(&["shadow-sessiond", "reload"]);
    for prog in ["shadow-bar", "shadow-sessiond", "shadow-displayd"] {
        let f = format!("/opt/shadow-tui/bin/{prog}");
        if pl.iter().any(|(k, x)| k == "replace" && *x == f)
            && sys::run(&["pgrep", "-x", prog]).0 == 0
        {
            sh_quiet(&["pkill", "-x", prog]);
            let _ = std::process::Command::new("sh")
                .args(["-c", &format!("setsid -f {prog} >/dev/null 2>&1")])
                .status();
            out(porcelain, "log", &format!("restarted {prog}"));
        }
    }
    let cfgs: Vec<&String> = pl
        .iter()
        .filter(|(k, _)| k == "config")
        .map(|(_, f)| f)
        .collect();
    for c in &cfgs {
        out(
            porcelain,
            "log",
            &format!("{c} changed upstream — yours is kept; the new version is {c}.shadow-new"),
        );
    }
    out(
        porcelain,
        "result",
        &format!("ok {n} file(s) updated to {}", &head[..8.min(head.len())]),
    );
    0
}

fn sh_quiet(cmd: &[&str]) {
    let _ = sys::run(cmd);
}

fn run_env(cmd: &[&str], cwd: &Path, env: &[(&str, String)]) -> i32 {
    let mut c = std::process::Command::new(cmd[0]);
    c.args(&cmd[1..])
        .current_dir(cwd)
        .stdout(std::process::Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    c.status().ok().and_then(|s| s.code()).unwrap_or(1)
}

fn stdin_is_tty() -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    // SAFETY: isatty only inspects the descriptor
    unsafe { isatty(0) == 1 }
}

/// Run with optional stdin text (the sudo password; never logged), streaming output lines.
fn run_input_stream(cmd: &[&str], input: Option<String>, mut each: impl FnMut(&str)) -> i32 {
    use std::io::{BufRead, BufReader, Write};
    let mut c = std::process::Command::new(cmd[0]);
    c.args(&cmd[1..])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    c.stdin(if input.is_some() {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::inherit()
    });
    let Ok(mut child) = c.spawn() else { return 127 };
    if let (Some(mut i), Some(mut text)) = (child.stdin.take(), input) {
        let _ = i.write_all(text.as_bytes());
        zero(&mut text);
    }
    let err = child.stderr.take();
    let t = std::thread::spawn(move || {
        err.map(|e| {
            BufReader::new(e)
                .lines()
                .map_while(Result::ok)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
    });
    if let Some(o) = child.stdout.take() {
        for l in BufReader::new(o).lines().map_while(Result::ok) {
            each(&l);
        }
    }
    for l in t.join().unwrap_or_default() {
        each(&l);
    }
    child.wait().ok().and_then(|s| s.code()).unwrap_or(1)
}

fn zero(s: &mut String) {
    // overwrite the password in place before the buffer is freed
    // SAFETY: zero bytes keep the String valid UTF-8
    unsafe {
        s.as_bytes_mut()
            .iter_mut()
            .for_each(|b| std::ptr::write_volatile(b, 0))
    };
}

fn copy_entry(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = to.with_file_name(format!(
        ".{}.shadow-tmp",
        to.file_name().unwrap_or_default().to_string_lossy()
    ));
    let _ = std::fs::remove_file(&tmp);
    let m = std::fs::symlink_metadata(from)?;
    if m.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, &tmp)?;
    } else {
        std::fs::copy(from, &tmp)?;
    }
    // rename is atomic: a running program keeps its old file until it restarts
    std::fs::rename(&tmp, to)
}

/// Root half of `apply`: back up, then replace / add / remove exactly the planned files.
pub fn install_staged(stage: &Path, planf: &Path, root: &Path) -> i32 {
    if !sys::is_root() && std::env::var_os("SHADOW_UPDATE_TEST_ROOT").is_none() {
        eprintln!("install-staged must run as root");
        return 1;
    }
    let ss = stage.to_string_lossy();
    if !stage.is_absolute()
        || !ss.ends_with("shadow-tui/stage")
        || !stage.join("opt/shadow-tui/share/FILES").is_file()
    {
        eprintln!("not a Shadow staging tree: {ss}");
        return 1;
    }
    let pl: Vec<(String, String)> = std::fs::read_to_string(planf)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            l.split_once(' ')
                .map(|(k, f)| (k.to_string(), f.to_string()))
        })
        .filter(|(_, f)| f.starts_with('/') && !f.contains("/../"))
        .collect();
    let backups = root.join(BACKUPS.trim_start_matches('/'));
    let bdir = backups.join(sys::now().to_string());
    if std::fs::create_dir_all(bdir.join("root")).is_err() {
        eprintln!("cannot create {}", bdir.display());
        return 1;
    }
    let live = |f: &str| root.join(f.trim_start_matches('/'));
    let staged = |f: &str| stage.join(f.trim_start_matches('/'));
    // 1. back up everything that will change (plan entries + version records)
    let mut journal = String::new();
    for (k, f) in &pl {
        if (k == "replace" || k == "remove")
            && copy_entry(&live(f), &bdir.join("root").join(f.trim_start_matches('/'))).is_err()
        {
            eprintln!("backup of {f} failed — nothing was changed");
            let _ = std::fs::remove_dir_all(&bdir);
            return 1;
        }
        journal.push_str(&format!("{k} {f}\n"));
    }
    for m in META {
        if live(m).exists() {
            let _ = copy_entry(&live(m), &bdir.join("root").join(m.trim_start_matches('/')));
            journal.push_str(&format!("meta {m}\n"));
        } else {
            journal.push_str(&format!("metanew {m}\n"));
        }
    }
    let _ = std::fs::write(bdir.join("journal"), &journal);
    // 2. apply
    let mut units = false;
    let mut done = 0;
    for (k, f) in &pl {
        let r = match k.as_str() {
            "replace" | "new" => copy_entry(&staged(f), &live(f)),
            "config" => copy_entry(&staged(f), &live(&format!("{f}.shadow-new"))),
            "remove" => std::fs::remove_file(live(f)),
            _ => Ok(()),
        };
        match r {
            Ok(()) => {
                done += 1;
                println!("{k} {f}");
                units |= f.starts_with("/usr/lib/systemd/");
            }
            Err(e) => println!("! {k} {f}: {e}"),
        }
    }
    for m in META {
        if staged(m).exists() {
            let _ = copy_entry(&staged(m), &live(m));
        }
    }
    // 3. system side effects
    if root == Path::new("/") {
        if units {
            sh_quiet(&["systemctl", "daemon-reload"]);
        }
        let touched = |b: &str| {
            pl.iter()
                .any(|(k, f)| k == "replace" && f == &format!("/opt/shadow-tui/bin/{b}"))
        };
        let mut restart: Vec<&str> = vec![];
        if touched("shadow-powerd") {
            restart.push("shadow-powerd.service");
        }
        if touched("shadow") {
            restart.extend(["shadow-netd.service", "shadow-healer.service"]);
        }
        if touched("shadow-sec") {
            restart.push("shadow-sec.service");
        }
        for u in restart {
            sh_quiet(&["systemctl", "try-restart", u]);
            println!("restarted {u}");
        }
        if sys::which("restorecon").is_some() {
            for (_, f) in &pl {
                if live(f).exists() {
                    sh_quiet(&["restorecon", &live(f).to_string_lossy()]);
                }
            }
        }
    }
    // keep the three most recent backups
    let mut all: Vec<PathBuf> = std::fs::read_dir(&backups)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    all.sort();
    while all.len() > 3 {
        let _ = std::fs::remove_dir_all(all.remove(0));
    }
    println!("{done} change(s) applied; backup in {}", bdir.display());
    0
}

/// Root: restore the most recent file-level update's backup.
pub fn undo(root: &Path) -> i32 {
    if !sys::is_root() && std::env::var_os("SHADOW_UPDATE_TEST_ROOT").is_none() {
        eprintln!("undo needs administrator rights: sudo shadow update undo");
        return 1;
    }
    let backups = root.join(BACKUPS.trim_start_matches('/'));
    let mut all: Vec<PathBuf> = std::fs::read_dir(&backups)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    all.sort();
    let Some(bdir) = all.pop() else {
        println!("no file-level update to undo");
        return 1;
    };
    let live = |f: &str| root.join(f.trim_start_matches('/'));
    let saved = |f: &str| bdir.join("root").join(f.trim_start_matches('/'));
    let mut n = 0;
    for l in std::fs::read_to_string(bdir.join("journal"))
        .unwrap_or_default()
        .lines()
    {
        let Some((k, f)) = l.split_once(' ') else {
            continue;
        };
        let r = match k {
            "replace" | "remove" | "meta" => copy_entry(&saved(f), &live(f)),
            "new" | "metanew" => std::fs::remove_file(live(f)).or(Ok(())),
            "config" => std::fs::remove_file(live(&format!("{f}.shadow-new"))).or(Ok(())),
            _ => Ok(()),
        };
        if r.is_ok() && !k.starts_with("meta") {
            n += 1;
        }
    }
    if root == Path::new("/") {
        sh_quiet(&["systemctl", "daemon-reload"]);
        for u in [
            "shadow-powerd.service",
            "shadow-netd.service",
            "shadow-healer.service",
        ] {
            sh_quiet(&["systemctl", "try-restart", u]);
        }
    }
    let _ = std::fs::remove_dir_all(&bdir);
    println!("undone: {n} file(s) restored to the version before the last update");
    0
}

#[cfg(test)]
mod file_update_tests {
    use super::*;

    fn w(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn only_changed_files_are_replaced_and_undo_restores() {
        let t = std::env::temp_dir().join(format!("upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let stage = t.join("cache/shadow-tui/stage");
        let root = t.join("root");
        // installed system
        w(&root.join("opt/shadow-tui/bin/shadow-bar"), "bar v1");
        w(&root.join("opt/shadow-tui/bin/shadow-net"), "net v1");
        w(&root.join("opt/shadow-tui/bin/old-tool"), "gone in v2");
        w(&root.join("etc/shadow-tui/netd.toml"), "mine");
        w(
            &root.join("opt/shadow-tui/share/BUILD_INFO"),
            "commit=aaa\n",
        );
        w(&root.join("opt/shadow-tui/share/FILES"), "/opt/shadow-tui/bin/shadow-bar\n/opt/shadow-tui/bin/shadow-net\n/opt/shadow-tui/bin/old-tool\n/etc/shadow-tui/netd.toml\n");
        // new version: bar changed, net identical, a new file, the config changed, old-tool dropped
        w(&stage.join("opt/shadow-tui/bin/shadow-bar"), "bar v2");
        w(&stage.join("opt/shadow-tui/bin/shadow-net"), "net v1");
        w(&stage.join("opt/shadow-tui/bin/shadow-update"), "new app");
        w(&stage.join("etc/shadow-tui/netd.toml"), "shipped v2");
        w(
            &stage.join("opt/shadow-tui/share/BUILD_INFO"),
            "commit=bbb\n",
        );
        w(&stage.join("opt/shadow-tui/share/FILES"), "/opt/shadow-tui/bin/shadow-bar\n/opt/shadow-tui/bin/shadow-net\n/opt/shadow-tui/bin/shadow-update\n/etc/shadow-tui/netd.toml\n");
        // equal permissions on both sides
        use std::os::unix::fs::PermissionsExt;
        for f in ["opt/shadow-tui/bin/shadow-net"] {
            std::fs::set_permissions(root.join(f), std::fs::Permissions::from_mode(0o755)).unwrap();
            std::fs::set_permissions(stage.join(f), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let pl = plan(&stage, &root);
        let has = |k: &str, f: &str| pl.contains(&(k.to_string(), f.to_string()));
        assert!(has("replace", "/opt/shadow-tui/bin/shadow-bar"));
        assert!(has("new", "/opt/shadow-tui/bin/shadow-update"));
        assert!(has("config", "/etc/shadow-tui/netd.toml"));
        assert!(has("remove", "/opt/shadow-tui/bin/old-tool"));
        assert!(
            !pl.iter().any(|(_, f)| f.ends_with("shadow-net")),
            "unchanged files are left alone"
        );
        assert_eq!(pl.len(), 4);
        let planf = t.join("plan");
        std::fs::write(
            &planf,
            pl.iter()
                .map(|(k, f)| format!("{k} {f}\n"))
                .collect::<String>(),
        )
        .unwrap();
        std::env::set_var("SHADOW_UPDATE_TEST_ROOT", "1");
        assert_eq!(install_staged(&stage, &planf, &root), 0);
        let rd =
            |f: &str| std::fs::read_to_string(root.join(f)).unwrap_or_else(|_| "<none>".into());
        assert_eq!(rd("opt/shadow-tui/bin/shadow-bar"), "bar v2");
        assert_eq!(rd("opt/shadow-tui/bin/shadow-update"), "new app");
        assert_eq!(
            rd("etc/shadow-tui/netd.toml"),
            "mine",
            "your config is never overwritten"
        );
        assert_eq!(rd("etc/shadow-tui/netd.toml.shadow-new"), "shipped v2");
        assert_eq!(rd("opt/shadow-tui/bin/old-tool"), "<none>");
        assert_eq!(rd("opt/shadow-tui/share/BUILD_INFO"), "commit=bbb\n");
        assert_eq!(undo(&root), 0);
        assert_eq!(rd("opt/shadow-tui/bin/shadow-bar"), "bar v1");
        assert_eq!(rd("opt/shadow-tui/bin/shadow-update"), "<none>");
        assert_eq!(rd("opt/shadow-tui/bin/old-tool"), "gone in v2");
        assert_eq!(rd("etc/shadow-tui/netd.toml.shadow-new"), "<none>");
        assert_eq!(rd("opt/shadow-tui/share/BUILD_INFO"), "commit=aaa\n");
        std::env::remove_var("SHADOW_UPDATE_TEST_ROOT");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn edited_config_is_left_alone_when_the_default_did_not_change() {
        let t = std::env::temp_dir().join(format!("updcfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let (stage, root) = (t.join("stage"), t.join("root"));
        let list = "/etc/shadow-tui/netd.toml\n";
        w(&stage.join("opt/shadow-tui/share/FILES"), list);
        w(&root.join("opt/shadow-tui/share/FILES"), list);
        w(&stage.join("etc/shadow-tui/netd.toml"), "default v1");
        w(&root.join("opt/shadow-tui/share/etc-defaults/shadow-tui/netd.toml"), "default v1");
        w(&root.join("etc/shadow-tui/netd.toml"), "default v1 + my edit");
        let p0 = plan(&stage, &root);
        assert!(p0.is_empty(), "unchanged default: no .shadow-new: {p0:?}");
        w(&stage.join("etc/shadow-tui/netd.toml"), "default v2");
        assert_eq!(plan(&stage, &root), vec![("config".to_string(), "/etc/shadow-tui/netd.toml".to_string())]);
        w(&root.join("etc/shadow-tui/netd.toml.shadow-new"), "default v2");
        assert!(plan(&stage, &root).is_empty(), "already offered");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn dependency_lists_are_parsed_across_lines() {
        let a = deps_of("pkgname=x\n  depends=(niri mesa\n           pam)  # c\nmakedepends=(cargo)\n", "Requires: niri\nBuildRequires: cargo >= 1.89\n");
        let b = deps_of("pkgname=x\n  depends=(niri mesa\n           pam newdep)\nmakedepends=(cargo)\n", "Requires: niri\nBuildRequires: cargo >= 1.89\n");
        assert!(a.contains(&"pam".to_string()) && a.contains(&"BuildRequires: cargo >= 1.89".to_string()));
        assert_ne!(a, b);
        assert_eq!(a, deps_of("depends=(pam niri mesa)\nmakedepends=(cargo)", "BuildRequires:   cargo >= 1.89\nRequires: niri"));
    }

    #[test]
    fn components_and_staging_guard() {
        assert_eq!(component("bar/src/main.rs"), "shadow-bar");
        assert!(component("PKGBUILD").contains("nothing installed"));
        // refused either way: not root (or) not a staging tree
        assert_eq!(
            install_staged(Path::new("/tmp/evil"), Path::new("/tmp/x"), Path::new("/")),
            1
        );
    }
}
