//! shadow-sync — declarative state + GitHub synchronisation (spec §6.2).
//!
//! The state directory ~/.config/shadow-state is itself the git repository:
//!   config.toml        core UI dimensions, refresh thresholds, keybindings
//!   term.conf ai.conf  optional per-tool settings
//!   npu_weights.bin    learned habits: power-mode Markov tables and launcher time-of-day counts
//!   healed_patches/    history of verified self-repaired modules (patches from shadow-healer)
//!   secrets.age        Wi-Fi credentials, encrypted with age to your SSH key (or a passphrase)
//!   etc/ lists/        system settings, package list, enabled services, Wi-Fi profile names
//! `shadow-sync push` commits and pushes (repos are created private); on a wiped machine
//! `shadow-sync bootstrap git@github.com:you/shadow-state.git` restores everything.
//! Plaintext secrets never enter the repo: every text file is scanned before a commit.
use crate::sys;
use std::path::{Path, PathBuf};

pub fn repo_dir() -> PathBuf {
    std::env::var_os("SHADOW_BACKUP_DIR").map(PathBuf::from).unwrap_or_else(crate::st::state_dir)
}

/// Files in the state dir that are restored on bootstrap.
const USER_FILES: [&str; 5] = ["config.toml", "term.conf", "ai.conf", "npu_weights.bin", "secrets.age"];
const ETC_FILES: [&str; 5] = ["powerd.conf", "known-bad.conf", "greeter.conf", "netd.toml", "healer.toml"];
const UNITS_SYSTEM: [&str; 6] = ["shadow-powerd.service", "shadow-netd.service", "shadow-healer.service", "shadow-audiod.service", "shadow-sec.service", "greetd.service"];
const UNITS_USER: [&str; 1] = ["shadow-sync.timer"];
/// binary / encrypted files the secret scanner does not read
const OPAQUE: [&str; 2] = ["secrets.age", "npu_weights.bin"];

/// Lines that look like secrets. Kept deliberately broad: a false alarm costs a re-run,
/// a leak cannot be undone (the repo may be on GitHub).
pub fn secret_hit(text: &str) -> Option<String> {
    // token formats: always a secret
    const TOKENS: [&str; 7] = ["-----BEGIN", "ghp_", "github_pat_", "glpat-", "xoxb-", "xoxp-", "AKIA"];
    // words that are a secret when they carry a value ("password = x", "\"psk\": \"x\"")
    const KEYS: [&str; 7] = ["password", "passwd", "psk", "secret", "token", "api_key", "apikey"];
    for (n, line) in text.lines().enumerate() {
        if let Some(t) = TOKENS.iter().find(|t| line.contains(*t)) {
            return Some(format!("line {}: looks like a key or token ({t}…)", n + 1));
        }
        let l = line.to_lowercase();
        for k in KEYS {
            if let Some(i) = l.find(k) {
                let rest = l[i + k.len()..].trim_start_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
                if rest.starts_with('=') || rest.starts_with(':') {
                    return Some(format!("line {}: has a \"{k}\" value", n + 1));
                }
            }
        }
    }
    None
}

fn git(dir: &Path, args: &[&str]) -> (i32, String, String) {
    let d = dir.to_string_lossy().to_string();
    let mut v = vec!["git", "-C", d.as_str()];
    v.extend_from_slice(args);
    sys::run(&v)
}

fn copy_into(src: &Path, dst: &Path) -> Result<bool, String> {
    if !src.is_file() {
        return Ok(false);
    }
    if let Some(p) = dst.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::copy(src, dst).map_err(|e| format!("{}: {e}", src.display()))?;
    Ok(true)
}

/// Write the snapshot into the repo working tree. Returns the list of files written.
pub fn snapshot(dir: &Path) -> Result<Vec<String>, String> {
    let mut wrote: Vec<String> = vec![];
    // replace managed subtrees so deletions are recorded too
    for sub in ["etc", "lists", "healed_patches"] {
        let _ = std::fs::remove_dir_all(dir.join(sub));
    }
    for f in ["config.toml", "term.conf", "ai.conf"] {
        if dir.join(f).is_file() {
            wrote.push(f.into());
        }
    }
    for f in ETC_FILES {
        if copy_into(&sys::p(&format!("etc/shadow-tui/{f}")), &dir.join("etc").join(f))? {
            wrote.push(format!("etc/{f}"));
        }
    }
    // learned habits: power-mode tables (shadow-powerd) + launcher time-of-day counts, one bundle
    let mut weights: Vec<u8> = b"SHADOW-WEIGHTS-1\n".to_vec();
    for (tag, p) in [("powerd", sys::p("var/lib/shadow-powerd/learn.state")), ("launcher", sys::state_dir().join("launcher.hours")), ("launcher-usage", sys::state_dir().join("launcher.usage"))] {
        if let Ok(b) = std::fs::read(&p) {
            weights.extend(format!("--- {tag} {}\n", b.len()).as_bytes());
            weights.extend(b);
            weights.push(b'\n');
        }
    }
    std::fs::write(dir.join("npu_weights.bin"), &weights).map_err(|e| e.to_string())?;
    // verified self-repairs: one patch file per healer commit
    let mods = crate::healer::modules_dir();
    if mods.join(".git").exists() {
        let out = dir.join("healed_patches");
        let _ = std::fs::create_dir_all(&out);
        let (c, _, _) = sys::run(&["git", "-C", &mods.to_string_lossy(), "format-patch", "-q", "--author=healer@shadow-tui.local", "--root", "-o", &out.to_string_lossy()]);
        if c == 0 {
            wrote.extend(std::fs::read_dir(&out).into_iter().flatten().flatten().map(|e| format!("healed_patches/{}", e.file_name().to_string_lossy())));
        }
    }
    match encrypt_secrets(dir) {
        Ok(Some(n)) => eprintln!("sync: {n} Wi-Fi credential(s) encrypted into secrets.age"),
        Ok(None) => {}
        Err(e) => eprintln!("sync: secrets not saved ({e})"),
    }
    std::fs::create_dir_all(dir.join("lists")).map_err(|e| e.to_string())?;
    let (c1, native, _) = sys::run(&["pacman", "-Qqen"]);
    let (c2, foreign, _) = sys::run(&["pacman", "-Qqem"]);
    if c1 == 0 {
        std::fs::write(dir.join("lists/packages.txt"), native + "\n").map_err(|e| e.to_string())?;
        wrote.push("lists/packages.txt".into());
    }
    if c2 == 0 || !foreign.is_empty() {
        std::fs::write(dir.join("lists/packages-aur.txt"), foreign + "\n").map_err(|e| e.to_string())?;
        wrote.push("lists/packages-aur.txt".into());
    }
    let mut units = String::new();
    for u in UNITS_SYSTEM {
        if sys::run(&["systemctl", "is-enabled", "-q", u]).0 == 0 {
            units.push_str(&format!("system {u}\n"));
        }
    }
    for u in UNITS_USER {
        if sys::run(&["systemctl", "--user", "is-enabled", "-q", u]).0 == 0 {
            units.push_str(&format!("user {u}\n"));
        }
    }
    std::fs::write(dir.join("lists/services.txt"), units).map_err(|e| e.to_string())?;
    wrote.push("lists/services.txt".into());
    // Wi-Fi: profile names + campus preset/identity only — never secrets
    let (code, out, _) = sys::run(&["nmcli", "-t", "-f", "NAME,TYPE", "connection", "show"]);
    if code == 0 {
        let mut w = String::from("# Wi-Fi profiles managed by Shadow TUI (no passwords). Restore asks for them.\n");
        for l in out.lines() {
            let name = l.rsplit_once(':').map(|(n, _)| n.replace("\\:", ":")).unwrap_or_default();
            if name.starts_with("SN ") {
                let ident = sys::run(&["nmcli", "-s", "-g", "802-1x.identity", "connection", "show", "id", &name]).1;
                w.push_str(&format!("{name}\t{}\n", ident.trim()));
            }
        }
        std::fs::write(dir.join("lists/wifi.txt"), w).map_err(|e| e.to_string())?;
        wrote.push("lists/wifi.txt".into());
    }
    let product = sys::read("sys/class/dmi/id/product_name").unwrap_or_default();
    let host = sys::read("etc/hostname").or_else(|| sys::read("proc/sys/kernel/hostname")).unwrap_or_default();
    std::fs::write(dir.join("MANIFEST"), format!(
        "Shadow TUI Restricted state\nmachine={product}\nhost={host}\ndate={}\nversion={}\nsource={}\n\nRestore on a fresh Arch install:\n  shadow-sync bootstrap <this repo URL>\n",
        sys::run(&["date", "-Iseconds"]).1, env!("CARGO_PKG_VERSION"), package_source())).map_err(|e| e.to_string())?;
    if !dir.join(".gitignore").exists() {
        std::fs::write(dir.join(".gitignore"), "*.tmp\n*.secret\n*.plain\n.sync-status\n").map_err(|e| e.to_string())?;
    }
    // secret scan: nothing secret-looking may be committed in the clear
    for f in wrote.iter().filter(|f| !OPAQUE.contains(&f.as_str())) {
        let text = std::fs::read_to_string(dir.join(f)).unwrap_or_default();
        if let Some(hit) = secret_hit(&text) {
            return Err(format!("{f} {hit} — not backing up. Remove the secret from the file (or move it elsewhere) and try again."));
        }
    }
    Ok(wrote)
}

/// where the installed package was built from (BUILD_INFO), so bootstrap can rebuild it
fn package_source() -> String {
    std::fs::read_to_string(crate::st::share().join("BUILD_INFO")).unwrap_or_default().lines().find_map(|l| l.strip_prefix("repo=").map(String::from)).unwrap_or_default()
}

fn age_recipient() -> Option<String> {
    crate::st::get("sync", "age_recipient").filter(|r| !r.is_empty()).or_else(|| {
        let k = crate::st::home().join(".ssh/id_ed25519.pub");
        k.exists().then(|| k.to_string_lossy().into_owned())
    })
}

/// Wi-Fi credentials of the SN profiles → age-encrypted secrets.age (to the SSH key or recipient file)
pub fn encrypt_secrets(dir: &Path) -> Result<Option<usize>, String> {
    if sys::which("age").is_none() {
        return Err("age is not installed (pacman -S age)".into());
    }
    let Some(rcpt) = age_recipient() else { return Err("no recipient: set [sync] age_recipient or create ~/.ssh/id_ed25519".into()) };
    let (code, out, _) = sys::run(&["nmcli", "-t", "-f", "NAME,TYPE", "connection", "show"]);
    if code != 0 {
        return Ok(None);
    }
    let mut plain = String::from("# shadow-sync secrets v1: profile<TAB>key<TAB>value\n");
    let mut n = 0;
    for l in out.lines().filter(|l| l.ends_with(":802-11-wireless")) {
        let name = l.rsplit_once(':').map(|(n, _)| n.replace("\\:", ":")).unwrap_or_default();
        if !name.starts_with("SN ") {
            continue;
        }
        for key in ["802-11-wireless.ssid", "802-1x.identity", "802-1x.password", "wifi-sec.psk", "802-1x.domain-suffix-match"] {
            let v = sys::run(&["nmcli", "-s", "-g", key, "connection", "show", "id", &name]).1;
            if !v.trim().is_empty() {
                plain.push_str(&format!("{name}\t{key}\t{}\n", v.trim()));
            }
        }
        n += 1;
    }
    if n == 0 {
        return Ok(None);
    }
    let flag = if rcpt.ends_with(".pub") || Path::new(&rcpt).is_file() { "-R" } else { "-r" };
    let target = dir.join("secrets.age");
    let (c, _, e) = sys::run_input(&["age", "-e", "-a", flag, &rcpt, "-o", &target.to_string_lossy()], Some(&plain));
    if c != 0 {
        return Err(format!("age: {e}"));
    }
    Ok(Some(n))
}

/// decrypt secrets.age with the SSH key (or interactively with a passphrase) and recreate profiles
fn restore_secrets(dir: &Path) {
    let f = dir.join("secrets.age");
    if !f.exists() || sys::which("age").is_none() {
        return;
    }
    let key = crate::st::home().join(".ssh/id_ed25519");
    let out = if key.exists() {
        sys::run(&["age", "-d", "-i", &key.to_string_lossy(), &f.to_string_lossy()])
    } else {
        // passphrase-encrypted: let age prompt on the terminal
        let o = std::process::Command::new("age").arg("-d").arg(&f).stderr(std::process::Stdio::inherit()).output();
        match o { Ok(o) => (o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned(), String::new()), Err(e) => (1, String::new(), e.to_string()) }
    };
    if out.0 != 0 {
        println!("secrets.age could not be decrypted ({}) — Wi-Fi profiles must be re-entered", out.2.trim());
        return;
    }
    let mut profiles: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> = Default::default();
    for l in out.1.lines().filter(|l| !l.starts_with('#')) {
        let p: Vec<&str> = l.splitn(3, '\t').collect();
        if p.len() == 3 {
            profiles.entry(p[0].into()).or_default().insert(p[1].into(), p[2].into());
        }
    }
    for (name, kv) in profiles {
        let ssid = kv.get("802-11-wireless.ssid").cloned().unwrap_or_else(|| name.trim_start_matches("SN ").to_string());
        let ok = match (kv.get("802-1x.identity"), kv.get("802-1x.password"), kv.get("wifi-sec.psk")) {
            (Some(id), Some(pw), _) => {
                let preset = if ssid.eq_ignore_ascii_case("eduroam") { "eduroam" } else { "ucalgary" };
                crate::wifi::setup_campus(preset, id, pw, kv.get("802-1x.domain-suffix-match").map(|s| s.as_str()), None).is_ok()
            }
            (_, _, Some(psk)) => crate::wifi::connect_psk(&ssid, psk, None),
            _ => false,
        };
        println!("  Wi-Fi {name}: {}", if ok { "restored" } else { "could not be recreated (enter it in shadow-net)" });
    }
}

fn mark(status: &str) {
    let _ = std::fs::write(repo_dir().join(".sync-status"), format!("{status} {}\n", sys::now()));
}

pub fn init(args: &[&str]) -> Result<String, String> {
    let dir = repo_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if !dir.join(".git").exists() {
        let (c, _, e) = git(&dir, &["init", "-q", "-b", "main"]);
        if c != 0 {
            return Err(format!("git init failed: {e}"));
        }
    }
    let opt = |k: &str| args.iter().position(|a| *a == k).and_then(|i| args.get(i + 1)).map(|s| s.to_string());
    let mut msg = format!("backup repository: {}", dir.display());
    if let Some(url) = opt("--remote") {
        let _ = git(&dir, &["remote", "remove", "origin"]);
        git(&dir, &["remote", "add", "origin", &url]);
        msg.push_str(&format!("\nremote: {url}"));
    } else if let Some(name) = opt("--github") {
        if sys::which("gh").is_none() {
            return Err("GitHub CLI (gh) not installed: pacman -S github-cli && gh auth login — or use --remote URL".into());
        }
        // private, always: the backup describes your machine
        let (c, out, e) = sys::run(&["gh", "repo", "create", &name, "--private", "--description", "Shadow TUI Restricted backup"]);
        if c != 0 && !e.contains("already exists") {
            return Err(format!("gh repo create failed: {e}"));
        }
        let url = out.lines().find(|l| l.starts_with("https://")).map(String::from)
            .unwrap_or_else(|| sys::run(&["gh", "repo", "view", &name, "--json", "url", "-q", ".url"]).1);
        let _ = git(&dir, &["remote", "remove", "origin"]);
        git(&dir, &["remote", "add", "origin", &format!("{}.git", url.trim_end_matches(".git"))]);
        msg.push_str(&format!("\nprivate GitHub repo: {url}"));
    }
    Ok(msg)
}

pub fn push(message: Option<&str>) -> Result<String, String> {
    let dir = repo_dir();
    if !dir.join(".git").exists() {
        init(&[])?;
    }
    let wrote = snapshot(&dir)?;
    git(&dir, &["add", "-A"]);
    let (_, changed, _) = git(&dir, &["status", "--porcelain"]);
    if changed.trim().is_empty() {
        mark("ok");
        return Ok(format!("no changes since the last sync ({} files checked)", wrote.len()));
    }
    let msg = message.map(String::from).unwrap_or_else(|| format!("backup {}", sys::run(&["date", "+%F %R"]).1));
    let (c, _, e) = git(&dir, &["-c", "user.name=Shadow TUI", "-c", "user.email=sync@shadow.local", "commit", "-q", "-m", &msg]);
    if c != 0 {
        mark("error");
        return Err(format!("commit failed: {e}"));
    }
    let n = changed.lines().count();
    if git(&dir, &["remote", "get-url", "origin"]).0 == 0 {
        let (c, _, e) = git(&dir, &["push", "-q", "-u", "origin", "main"]);
        if c != 0 {
            mark("dirty");
            return Ok(format!("saved locally ({n} changed files) but push failed: {} — run `shadow-sync push` again when online", e.lines().last().unwrap_or("")));
        }
        mark("ok");
        return Ok(format!("committed and pushed ({n} changed files)"));
    }
    mark("dirty");
    Ok(format!("saved locally ({n} changed files) — add a remote: shadow-sync init --github NAME"))
}

pub fn status() -> String {
    let dir = repo_dir();
    if !dir.join(".git").exists() {
        return "no state repository yet — `shadow-sync init --github shadow-state` (private) or `shadow-sync push` for a local one".into();
    }
    let last = git(&dir, &["log", "-1", "--format=%cd · %s", "--date=format:%F %R"]).1;
    let _ = &last;
    let remote = git(&dir, &["remote", "get-url", "origin"]).1;
    format!("repository : {}\nremote     : {}\nlast backup: {}", dir.display(), if remote.is_empty() { "(none)".into() } else { remote }, if last.is_empty() { "-".into() } else { last })
}

/// Restore from a repo URL or directory. `system`: also install packages, the Shadow runtime
/// and services (that is `bootstrap`).
pub fn restore(src: &str, system: bool, yes: bool) -> Result<(), String> {
    let dir = if Path::new(src).join("MANIFEST").exists() {
        PathBuf::from(src)
    } else {
        let tmp = sys::data_dir().join(format!("restore-{}", sys::now()));
        let t = tmp.to_string_lossy().to_string();
        // backups are pushed to `main`; fall back to the remote's default branch
        let (mut c, _, mut e) = sys::run(&["git", "clone", "-q", "--depth", "1", "--branch", "main", src, &t]);
        if c != 0 {
            let _ = std::fs::remove_dir_all(&tmp);
            (c, _, e) = sys::run(&["git", "clone", "-q", "--depth", "1", src, &t]);
        }
        if c != 0 {
            return Err(format!("could not clone {src}: {e}"));
        }
        tmp
    };
    if !dir.join("MANIFEST").exists() {
        return Err(format!("{src} is not a Shadow TUI state repository (no MANIFEST)"));
    }
    println!("{}", std::fs::read_to_string(dir.join("MANIFEST")).unwrap_or_default().lines().take(4).collect::<Vec<_>>().join("\n"));
    // keep what is there now
    let conf = crate::st::state_dir();
    let keep = sys::state_dir().join(format!("pre-restore-{}", sys::now()));
    for f in USER_FILES {
        let _ = copy_into(&conf.join(f), &keep.join(f));
    }
    let mut n = 0;
    for f in USER_FILES {
        if copy_into(&dir.join("config").join(f), &conf.join(f))? {
            n += 1;
        }
    }
    let _ = &mut n;
    if dir != conf {
        for f in USER_FILES {
            if copy_into(&dir.join(f), &conf.join(f))? {
                n += 1;
            }
        }
        // keep the clone's history: the state dir becomes the repository
        if !conf.join(".git").exists() {
            let _ = copy_tree(&dir.join(".git"), &conf.join(".git"));
        }
    }
    println!("restored {n} state files (your previous ones are in {})", keep.display());
    // learned habits back where the tools read them
    if let Ok(b) = std::fs::read(conf.join("npu_weights.bin")) {
        unpack_weights(&b);
    }
    if system {
        let pk = std::fs::read_to_string(dir.join("lists/packages.txt")).unwrap_or_default();
        let pkgs: Vec<&str> = pk.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        if !pkgs.is_empty() {
            println!("installing {} packages (already installed ones are skipped)…", pkgs.len());
            let mut cmd = vec!["sudo", "pacman", "-S", "--needed"];
            if yes {
                cmd.push("--noconfirm");
            }
            cmd.extend(pkgs.iter());
            let st = std::process::Command::new(cmd[0]).args(&cmd[1..]).status();
            if !st.map(|s| s.success()).unwrap_or(false) {
                println!("  some packages failed to install (renamed or removed from the repos?) — continuing");
            }
        }
        let aur = std::fs::read_to_string(dir.join("lists/packages-aur.txt")).unwrap_or_default();
        let aur: Vec<&str> = aur.lines().map(str::trim).filter(|l| !l.is_empty() && *l != "shadow-network" && *l != "shadow-tui-restricted").collect();
        if !aur.is_empty() {
            if let Some(helper) = ["paru", "yay"].iter().find(|h| sys::which(h).is_some()) {
                let mut cmd = vec![*helper, "-S", "--needed"];
                cmd.extend(aur.iter());
                let _ = std::process::Command::new(cmd[0]).args(&cmd[1..]).status();
            } else {
                println!("AUR packages to install with your AUR helper: {}", aur.join(" "));
            }
        }
        // the isolated runtime itself: rebuild the package from the recorded source
        if sys::run(&["pacman", "-Q", "shadow-tui-restricted"]).0 != 0 {
            let src = std::fs::read_to_string(dir.join("MANIFEST")).unwrap_or_default().lines().find_map(|l| l.strip_prefix("source=").map(String::from)).unwrap_or_default();
            if src.is_empty() {
                println!("the Shadow runtime is not installed and the source is unknown: clone the shadow-tui-restricted repo and run ./install.sh");
            } else {
                println!("deploying the Shadow runtime (/opt/shadow-tui) from {src}…");
                let tmp = std::env::temp_dir().join(format!("shadow-src-{}", sys::now()));
                if sys::run(&["git", "clone", "-q", "--depth", "1", &src, &tmp.to_string_lossy()]).0 == 0 {
                    let _ = std::process::Command::new("makepkg").args(["-si", "--noconfirm"]).current_dir(&tmp).status();
                }
            }
        }
        for f in ETC_FILES {
            let s = dir.join("etc").join(f);
            if s.is_file() {
                let _ = std::process::Command::new("sudo").args(["install", "-Dm644"]).arg(&s).arg(format!("/etc/shadow-tui/{f}")).status();
            }
        }
        for l in std::fs::read_to_string(dir.join("lists/services.txt")).unwrap_or_default().lines() {
            match l.split_once(' ') {
                Some(("system", u)) => { let _ = std::process::Command::new("sudo").args(["systemctl", "enable", u]).status(); }
                Some(("user", u)) => { sys::run(&["systemctl", "--user", "enable", u]); }
                _ => {}
            }
        }
    }
    if conf.join("secrets.age").exists() {
        restore_secrets(&conf);
    } else {
        let wifi = std::fs::read_to_string(dir.join("lists/wifi.txt")).unwrap_or_default();
        let campus: Vec<&str> = wifi.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()).collect();
        if !campus.is_empty() {
            println!("Wi-Fi profiles to recreate (no secrets.age in this repository):");
            for l in campus {
                let (name, ident) = l.split_once('\t').unwrap_or((l, ""));
                let preset = if name.contains("eduroam") { "eduroam" } else { "ucalgary" };
                println!("  {name}: shadow wifi campus {preset} --user {}", ident.split('@').next().unwrap_or(ident));
            }
        }
    }
    let _ = std::process::Command::new("shadow").args(["setup", "--yes"]).status();
    println!("restore complete — log out and back in (or reboot after a package install)");
    Ok(())
}

pub fn auto(on: bool) -> Result<String, String> {
    let verb = if on { "enable" } else { "disable" };
    let (c, _, e) = sys::run(&["systemctl", "--user", verb, "--now", "shadow-sync.timer"]);
    if c != 0 {
        return Err(e);
    }
    Ok(format!("daily automatic sync {}", if on { "on" } else { "off" }))
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)?.flatten() {
        let t = dst.join(e.file_name());
        if e.path().is_dir() { copy_tree(&e.path(), &t)? } else { std::fs::copy(e.path(), t)?; }
    }
    Ok(())
}

fn unpack_weights(b: &[u8]) {
    let text = String::from_utf8_lossy(b);
    let mut it = text.split("--- ").skip(1);
    for chunk in it.by_ref() {
        let (head, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
        let mut h = head.split_whitespace();
        let (Some(tag), Some(len)) = (h.next(), h.next().and_then(|l| l.parse::<usize>().ok())) else { continue };
        let body = &body.as_bytes()[..len.min(body.len())];
        let dest = match tag {
            "launcher" => sys::state_dir().join("launcher.hours"),
            "launcher-usage" => sys::state_dir().join("launcher.usage"),
            "powerd" => { println!("power-mode habits restored at the next `sudo shadow-powerd learn import`"); sys::state_dir().join("powerd-learn.import") }
            _ => continue,
        };
        let _ = std::fs::create_dir_all(dest.parent().unwrap());
        let _ = std::fs::write(dest, body);
    }
}

pub fn main(args: &[&str]) -> i32 {
    let out = |r: Result<String, String>| match r {
        Ok(s) => { println!("{s}"); 0 }
        Err(e) => { eprintln!("shadow-sync: {e}"); 1 }
    };
    match args {
        ["init", rest @ ..] => out(init(rest)),
        ["push", rest @ ..] => {
            let m = rest.iter().position(|x| *x == "-m").and_then(|i| rest.get(i + 1)).copied();
            out(push(m))
        }
        ["status"] | [] => { println!("{}", status()); 0 }
        ["bootstrap", src, rest @ ..] => out(restore(src, true, rest.contains(&"--yes")).map(|_| String::new())),
        ["restore", src, rest @ ..] => out(restore(src, rest.contains(&"--system"), rest.contains(&"--yes")).map(|_| String::new())),
        ["auto", "on"] => out(auto(true)),
        ["auto", "off"] => out(auto(false)),
        _ => {
            eprintln!("usage: shadow-sync init [--github NAME | --remote URL] | push [-m MSG] | status | bootstrap URL [--yes] | restore URL|DIR [--system] | auto on|off");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_are_caught() {
        assert!(secret_hit("ssid=x\npsk=hunter2\n").is_some());
        assert!(secret_hit("{\"password\": \"x\"}").is_some());
        assert!(secret_hit("token = ghp_abcdef").is_some());
        assert!(secret_hit("-----BEGIN OPENSSH PRIVATE KEY-----").is_some());
        assert!(secret_hit("{\"wifi\": {\"self_correct\": true, \"allow_unvalidated\": false}}").is_none());
        assert!(secret_hit("Mod+Return | Terminal | exec $terminal").is_none());
        assert!(secret_hit("[wifi]\nself_correct = true\n[sync]\nage_recipient = \"/home/x/.ssh/id_ed25519.pub\"").is_none());
    }
}
