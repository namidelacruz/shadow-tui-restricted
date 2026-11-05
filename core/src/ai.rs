//! `shadow ai` — the local assistant and the approval-gated self-correction pipeline.
//!
//! The model runs on this machine (OpenVINO Model Server on the Intel NPU is the intended
//! backend; any OpenAI-compatible server on localhost/LAN works). Nothing goes to the internet.
//!
//! Self-correction never patches the system on its own:
//!   1. `collect`  gathers errors from Shadow TUI's logs and journals into issues
//!   2. `propose`  asks the model for a minimal fix as a diff, applies it to a scratch copy of the
//!                 source and runs the fast tests for the touched parts
//!   3. `review`   shows you the diff, the error it addresses and the test results
//!   4. `approve`  (you, typing APPROVE) — builds the full package with all its tests and installs
//!                 it through the normal update path, keeping the previous version for rollback
use crate::http;
use crate::json::{self, Value};
use crate::sys;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn conf() -> BTreeMap<String, String> {
    let mut m: BTreeMap<String, String> = [
        ("endpoint", "http://127.0.0.1:8000/v3"),
        ("model", "shadow-assistant"),
        ("device", "NPU"),
        ("timeout", "600"),
    ].iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    for (k, v) in sys::read_kv(&sys::config_dir().join("ai.conf")) {
        m.insert(k, v);
    }
    m
}

fn ai_dir() -> PathBuf {
    sys::state_dir().join("ai")
}

fn src_dir() -> PathBuf {
    std::env::var_os("SHADOW_SRC").map(PathBuf::from).unwrap_or_else(|| sys::cache_dir().join("src"))
}

// ---------------------------------------------------------------- model access
pub fn chat(system: &str, user: &str) -> Result<String, String> {
    let c = conf();
    let body = Value::Obj(vec![
        ("model".into(), Value::Str(c["model"].clone())),
        ("temperature".into(), Value::Num(0.1)),
        ("max_tokens".into(), Value::Num(2048.0)),
        ("messages".into(), Value::Arr(vec![
            Value::Obj(vec![("role".into(), Value::Str("system".into())), ("content".into(), Value::Str(system.into()))]),
            Value::Obj(vec![("role".into(), Value::Str("user".into())), ("content".into(), Value::Str(user.into()))]),
        ])),
    ]);
    let url = format!("{}/chat/completions", c["endpoint"].trim_end_matches('/'));
    let (code, text) = http::request("POST", &url, Some(&body.pretty()), c["timeout"].parse().unwrap_or(600))?;
    if code != 200 {
        return Err(format!("model server answered HTTP {code}: {}", text.chars().take(200).collect::<String>()));
    }
    let v = json::parse(&text).map_err(|e| format!("bad JSON from the model server: {e}"))?;
    match v.get("choices") {
        Some(Value::Arr(a)) if !a.is_empty() => a[0].path_str(&["message", "content"]).ok_or_else(|| "no message in the reply".into()),
        _ => Err("no choices in the reply".into()),
    }
}

pub fn status() -> String {
    let c = conf();
    let url = format!("{}/models", c["endpoint"].trim_end_matches('/'));
    let server = match http::request("GET", &url, None, 5) {
        Ok((200, body)) => {
            let names: Vec<String> = json::parse(&body).ok().and_then(|v| match v.get("data") {
                Some(Value::Arr(a)) => Some(a.iter().filter_map(|m| m.path_str(&["id"])).collect()),
                _ => None,
            }).unwrap_or_default();
            format!("reachable · models: {}", if names.is_empty() { "?".into() } else { names.join(", ") })
        }
        Ok((code, _)) => format!("answered HTTP {code}"),
        Err(e) => e,
    };
    let issues = list_issues().len();
    let props = list_proposals();
    let pending = props.iter().filter(|(_, st)| st == "tested-ok" || st == "tests-failed").count();
    format!(
        "endpoint : {} ({})\nmodel    : {} on {}\nserver   : {server}\nissues   : {issues} collected · proposals awaiting your review: {pending}\nnpu      : run `shadow-powerd npu` for driver/runtime readiness",
        c["endpoint"], if http::parse_url(&c["endpoint"]).map(|u| http::is_local(&u.host)).unwrap_or(false) { "local" } else { "NOT local — refused" },
        c["model"], c["device"]
    )
}

pub fn setup(args: &[&str]) -> Result<String, String> {
    let opt = |k: &str| args.iter().position(|a| *a == k).and_then(|i| args.get(i + 1)).map(|s| s.to_string());
    let mut c = conf();
    if let Some(e) = opt("--endpoint") {
        let u = http::parse_url(&e)?;
        if !http::is_local(&u.host) {
            return Err("the endpoint must be on this machine or your LAN".into());
        }
        c.insert("endpoint".into(), e);
    }
    if let Some(m) = opt("--model") {
        c.insert("model".into(), m);
    }
    if let Some(d) = opt("--device") {
        c.insert("device".into(), d);
    }
    let text: String = std::iter::once("# Shadow TUI local assistant (shadow ai)\n".to_string())
        .chain(c.iter().map(|(k, v)| format!("{k} = {v}\n"))).collect();
    sys::atomic_write(&sys::config_dir().join("ai.conf"), &text).map_err(|e| e.to_string())?;
    Ok(format!("saved {}\n\n{}", sys::config_dir().join("ai.conf").display(), status()))
}

// ---------------------------------------------------------------- issues
#[derive(Debug, Clone)]
pub struct Issue {
    pub id: String,
    pub source: String,
    pub text: String,
}

fn sig(s: &str) -> String {
    // stable id: FNV-1a of the error's first and last meaningful lines
    let key: String = s.lines().filter(|l| !l.trim().is_empty()).take(1).chain(s.lines().rev().filter(|l| !l.trim().is_empty()).take(1)).collect();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in key.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")[..10].to_string()
}

/// Pull Python tracebacks and Rust panics out of a log text.
pub fn extract(text: &str, source: &str) -> Vec<Issue> {
    let mut out = vec![];
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if l.contains("Traceback (most recent call last)") {
            let mut block = vec![l];
            i += 1;
            while i < lines.len() && (lines[i].starts_with(' ') || lines[i].starts_with('\t') || lines[i].contains("File \"")) {
                block.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                block.push(lines[i]); // the exception line
            }
            let t = block.join("\n");
            out.push(Issue { id: sig(&t), source: source.into(), text: t });
        } else if l.contains("panicked at") {
            let mut block = vec![l];
            if i + 1 < lines.len() {
                block.push(lines[i + 1]);
            }
            let t = block.join("\n");
            out.push(Issue { id: sig(&t), source: source.into(), text: t });
        }
        i += 1;
    }
    out
}

pub fn collect() -> Result<String, String> {
    let dir = ai_dir().join("issues");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut found: Vec<Issue> = vec![];
    let since = "--since=-48h";
    let (_, u, _) = sys::run(&["journalctl", "--user", "-o", "cat", "--no-pager", since, "-u", "shadow*"]);
    found.extend(extract(&u, "user journal"));
    let (_, s, _) = sys::run(&["journalctl", "-o", "cat", "--no-pager", since, "-u", "shadow-powerd.service", "-u", "shadow-netd.service", "-u", "shadow-healer.service", "-u", "shadow-wifi-device.service"]);
    found.extend(extract(&s, "system journal"));
    if let Ok(rd) = std::fs::read_dir(sys::state_dir()) {
        for e in rd.flatten() {
            if e.path().extension().map_or(false, |x| x == "log") {
                let t = std::fs::read_to_string(e.path()).unwrap_or_default();
                found.extend(extract(&t, &e.file_name().to_string_lossy()));
            }
        }
    }
    let mut new = 0;
    for i in &found {
        let p = dir.join(format!("{}.txt", i.id));
        if !p.exists() {
            new += 1;
            let _ = std::fs::write(&p, format!("source: {}\n\n{}\n", i.source, i.text));
        }
    }
    Ok(format!("{} error(s) found, {new} new — `shadow ai issues` to list, `shadow ai propose ID` to draft a fix", found.len()))
}

pub fn list_issues() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(ai_dir().join("issues"))
        .map(|rd| rd.flatten().filter_map(|e| {
            let id = e.path().file_stem()?.to_string_lossy().into_owned();
            let t = std::fs::read_to_string(e.path()).ok()?;
            let last = t.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
            Some((id, last))
        }).collect())
        .unwrap_or_default();
    v.sort();
    v
}

// ---------------------------------------------------------------- proposals
/// Map an error to source files in the repository (relative paths).
pub fn files_for(issue: &str, repo: &Path) -> Vec<(String, Option<usize>)> {
    let mut out: Vec<(String, Option<usize>)> = vec![];
    for l in issue.lines() {
        // Rust: panicked at src/files.rs:123:9
        if let Some(i) = l.find("panicked at ") {
            let loc = l[i + 12..].trim().trim_end_matches(':');
            let mut parts = loc.split(':');
            let rel = parts.next().unwrap_or("");
            let line = parts.next().and_then(|x| x.parse().ok());
            for crate_dir in ["ui", "bar", "apps", "desktop", "niri", "greet", "term", "powerd", "core", "lua", "sec"] {
                if repo.join(crate_dir).join(rel).is_file() {
                    out.push((format!("{crate_dir}/{rel}"), line));
                }
            }
        }
    }
    out.dedup();
    out.retain(|(f, _)| repo.join(f).is_file());
    out
}

fn excerpt(path: &Path, line: Option<usize>) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let (a, b) = match line {
        Some(n) => (n.saturating_sub(60).max(1), (n + 60).min(lines.len())),
        None => (1, lines.len().min(200)),
    };
    (a..=b).filter_map(|i| lines.get(i - 1).map(|l| format!("{i:5} | {l}"))).collect::<Vec<_>>().join("\n")
}

/// Pull a unified diff out of a model reply (fenced or bare).
pub fn extract_diff(reply: &str) -> Option<String> {
    if reply.trim_start().starts_with("NO_FIX") {
        return None;
    }
    let body = if let Some(i) = reply.find("```") {
        let after = &reply[i + 3..];
        let after = after.split_once('\n').map(|(_, r)| r).unwrap_or(after);
        after.split("```").next().unwrap_or("")
    } else {
        reply
    };
    let start = body.find("diff --git").or_else(|| body.find("--- a/"))?;
    let d = body[start..].trim_end().to_string() + "\n";
    (d.contains("+++ ") && d.contains("@@")).then_some(d)
}

const REPAIR_PROMPT: &str = "You are the repair assistant for Shadow TUI Restricted, an Arch Linux desktop \
(Rust + Python). You get an error and the relevant source. Reply with ONLY a minimal unified diff in git format \
(paths relative to the repository root, a/ and b/ prefixes, correct @@ hunk headers) that fixes the root cause \
without changing behaviour elsewhere. Never add network access, never weaken security checks, never touch \
passwords or credentials, never delete tests. If you cannot fix it safely, reply with NO_FIX and one sentence why.";

fn ensure_src() -> Result<PathBuf, String> {
    let src = src_dir();
    if src.join(".git").exists() {
        return Ok(src);
    }
    let repo = std::fs::read_to_string(sys::config_dir().join("settings.json")).ok()
        .and_then(|t| json::parse(&t).ok()).and_then(|v| v.path_str(&["updates", "repo"])).unwrap_or_default();
    if repo.is_empty() {
        return Err("no source checkout: set your update repository (Settings → Updates) so fixes can be built from source".into());
    }
    let (c, _, e) = sys::run(&["git", "clone", "-q", "--depth", "1", &repo, &src.to_string_lossy()]);
    if c != 0 {
        return Err(format!("could not fetch the source: {e}"));
    }
    Ok(src)
}

/// Fast tests for the parts a diff touches (the full package build runs at approval).
pub fn quick_tests(work: &Path, touched: &[String]) -> (bool, String) {
    let mut log = String::new();
    let mut ok = true;
    let mut run = |label: &str, args: &[&str], dir: &Path| {
        let mut c = std::process::Command::new(args[0]);
        c.args(&args[1..]).current_dir(dir);
        let o = c.output();
        let (code, out) = match o {
            Ok(o) => (o.status.code().unwrap_or(1), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
            Err(e) => (127, e.to_string()),
        };
        log.push_str(&format!("== {label}: {}\n{}\n", if code == 0 { "ok" } else { "FAILED" }, out.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")));
        if code != 0 {
            ok = false;
        }
    };
    for (c, pkg) in [("ui", "shadow-ui"), ("bar", "shadow-bar"), ("apps", "shadow-apps"), ("desktop", "shadow-desktop"), ("niri", "shadow-niri"), ("greet", "shadow-greet"), ("term", "shadow-term"), ("powerd", "shadow-powerd"), ("core", "shadow"), ("lua", "shadow-lua"), ("sec", "shadow-sec")] {
        if touched.iter().any(|f| f.starts_with(&format!("{c}/"))) {
            run(&format!("{c} tests"), &["cargo", "test", "--offline", "--release", "-q", "-p", pkg], work);
        }
    }
    if touched.iter().any(|f| f.starts_with("system/") && !f.ends_with(".service") && !f.ends_with(".desktop")) {
        for f in touched.iter().filter(|f| f.starts_with("system/")) {
            run(&format!("sh -n {f}"), &["sh", "-n", f], work);
        }
    }
    if log.is_empty() {
        log.push_str("no fast tests for these files — the full package build at approval runs every test\n");
    }
    (ok, log)
}

fn touched_files(diff: &str) -> Vec<String> {
    let mut v: Vec<String> = diff.lines().filter_map(|l| l.strip_prefix("+++ b/")).map(|s| s.trim().to_string()).collect();
    v.dedup();
    v
}

pub fn propose(issue_id: &str) -> Result<String, String> {
    let ipath = ai_dir().join("issues").join(format!("{issue_id}.txt"));
    let issue = std::fs::read_to_string(&ipath).map_err(|_| format!("no issue {issue_id} — `shadow ai issues` lists them"))?;
    let src = ensure_src()?;
    let files = files_for(&issue, &src);
    let mut user = format!("ERROR\n=====\n{issue}\n");
    for (f, line) in &files {
        user.push_str(&format!("\nFILE {f} (line numbers shown; do not include them in the diff)\n{}\n", excerpt(&src.join(f), *line)));
    }
    if files.is_empty() {
        user.push_str("\n(no source file could be matched to this error)\n");
    }
    let reply = chat(REPAIR_PROMPT, &user)?;
    let n = list_proposals().len() + 1;
    let pdir = ai_dir().join("proposals").join(format!("{n:03}"));
    std::fs::create_dir_all(&pdir).map_err(|e| e.to_string())?;
    std::fs::write(pdir.join("issue.txt"), &issue).map_err(|e| e.to_string())?;
    std::fs::write(pdir.join("reply.txt"), &reply).map_err(|e| e.to_string())?;
    let set = |st: &str| { let _ = std::fs::write(pdir.join("status"), st); };
    let Some(diff) = extract_diff(&reply) else {
        set("no-fix");
        return Ok(format!("proposal {n:03}: the model did not offer a safe fix ({})", reply.lines().next().unwrap_or("").chars().take(120).collect::<String>()));
    };
    std::fs::write(pdir.join("fix.patch"), &diff).map_err(|e| e.to_string())?;
    // scratch copy of the source (git worktree), apply, test
    let work = sys::cache_dir().join("ai-work").join(format!("{n:03}"));
    let _ = std::fs::remove_dir_all(&work);
    let _ = sys::run(&["git", "-C", &src.to_string_lossy(), "worktree", "prune"]);
    let (c, _, e) = sys::run(&["git", "-C", &src.to_string_lossy(), "worktree", "add", "-q", "--detach", &work.to_string_lossy(), "HEAD"]);
    if c != 0 {
        set("error");
        return Err(format!("could not create a scratch copy: {e}"));
    }
    let patch = pdir.join("fix.patch").to_string_lossy().to_string();
    let (c, _, e) = sys::run(&["git", "-C", &work.to_string_lossy(), "apply", "--check", &patch]);
    if c != 0 {
        set("does-not-apply");
        return Ok(format!("proposal {n:03}: the model's diff does not apply cleanly ({}) — kept for reference, nothing changed", e.lines().next().unwrap_or("")));
    }
    sys::run(&["git", "-C", &work.to_string_lossy(), "apply", &patch]);
    let touched = touched_files(&diff);
    let (ok, log) = quick_tests(&work, &touched);
    std::fs::write(pdir.join("tests.log"), &log).map_err(|e| e.to_string())?;
    std::fs::write(pdir.join("worktree"), work.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    set(if ok { "tested-ok" } else { "tests-failed" });
    Ok(format!("proposal {n:03}: {} · touches {} · review it with `shadow ai review {n:03}`",
        if ok { "fast tests pass" } else { "TESTS FAIL" }, touched.join(", ")))
}

pub fn list_proposals() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(ai_dir().join("proposals"))
        .map(|rd| rd.flatten().map(|e| (e.file_name().to_string_lossy().into_owned(),
            std::fs::read_to_string(e.path().join("status")).unwrap_or_else(|_| "?".into()).trim().to_string())).collect())
        .unwrap_or_default();
    v.sort();
    v
}

pub fn review(n: &str) -> Result<String, String> {
    let p = ai_dir().join("proposals").join(n);
    let st = std::fs::read_to_string(p.join("status")).map_err(|_| format!("no proposal {n}"))?;
    let issue = std::fs::read_to_string(p.join("issue.txt")).unwrap_or_default();
    let diff = std::fs::read_to_string(p.join("fix.patch")).unwrap_or_else(|_| "(no diff)".into());
    let tests = std::fs::read_to_string(p.join("tests.log")).unwrap_or_default();
    Ok(format!("PROPOSAL {n} — status: {}\n\n── error ──\n{}\n\n── proposed change ──\n{diff}\n── fast tests ──\n{tests}\nApprove (builds the full package with all tests, installs, keeps a rollback): shadow ai approve {n}\nReject: shadow ai reject {n}",
        st.trim(), issue.trim()))
}

pub fn approve(n: &str, confirm: Option<&str>) -> Result<String, String> {
    let p = ai_dir().join("proposals").join(n);
    let st = std::fs::read_to_string(p.join("status")).map_err(|_| format!("no proposal {n}"))?;
    if st.trim() != "tested-ok" {
        return Err(format!("proposal {n} is '{}' — only a proposal whose tests pass can be approved", st.trim()));
    }
    // explicit human approval: typed, never a flag a script could pass by accident
    let typed = match confirm {
        Some(c) => c.to_string(),
        None => {
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() {
                return Err("approval must be typed at a terminal".into());
            }
            print!("{}\nType APPROVE to build and install this change: ", review(n)?);
            let _ = std::io::Write::flush(&mut std::io::stdout());
            let mut l = String::new();
            let _ = std::io::stdin().read_line(&mut l);
            l
        }
    };
    if typed.trim() != "APPROVE" {
        return Ok("not approved — nothing changed".into());
    }
    let work = PathBuf::from(std::fs::read_to_string(p.join("worktree")).map_err(|e| e.to_string())?.trim());
    let wd = work.to_string_lossy().to_string();
    sys::run(&["git", "-C", &wd, "-c", "user.name=Shadow TUI AI (approved)", "-c", "user.email=ai@shadow.local",
        "commit", "-q", "-am", &format!("AI fix {n} (approved by the user)")]);
    let _ = std::fs::write(p.join("status"), "approved");
    let st = std::process::Command::new("shadow").args(["update", "install-local", &wd]).status();
    let ok = st.map(|s| s.success()).unwrap_or(false);
    let _ = std::fs::write(p.join("status"), if ok { "installed" } else { "approved-build-failed" });
    Ok(if ok { format!("proposal {n} installed — roll back any time with: shadow update rollback") }
       else { format!("proposal {n}: the full build or its tests failed — nothing was installed") })
}

pub fn reject(n: &str) -> Result<String, String> {
    let p = ai_dir().join("proposals").join(n);
    std::fs::write(p.join("status"), "rejected").map_err(|_| format!("no proposal {n}"))?;
    if let Ok(w) = std::fs::read_to_string(p.join("worktree")) {
        let src = src_dir();
        sys::run(&["git", "-C", &src.to_string_lossy(), "worktree", "remove", "--force", w.trim()]);
    }
    Ok(format!("proposal {n} rejected"))
}

pub fn ask(q: &str) -> Result<String, String> {
    let ctx = format!(
        "Machine state:\n{}\n\nWi-Fi:\n{}\n",
        sys::run(&["shadow-powerd", "status"]).1,
        sys::run(&["shadow", "wifi", "status"]).1
    );
    chat("You are the built-in assistant of Shadow TUI Restricted (Arch Linux, Dell 14 Plus, keyboard-first desktop on niri). \
          Answer briefly and concretely. Prefer the system's own commands (shadow, shadow-powerd, shadow-displayd, shadow-sync, shadow-healer, shadowctl) when they solve the problem. \
          Never ask for passwords.", &format!("{ctx}\nQuestion: {q}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panic_extraction_and_mapping() {
        let log = "noise\nthread 'main' panicked at src/wifi.rs:42:9:\nindex out of bounds: the len is 3 but the index is 3\nmore\n";
        let is = extract(log, "t");
        assert_eq!(is.len(), 1);
        let d = std::env::temp_dir().join(format!("sn-ai-{}", std::process::id()));
        std::fs::create_dir_all(d.join("core/src")).unwrap();
        std::fs::write(d.join("core/src/wifi.rs"), "a\n").unwrap();
        assert_eq!(files_for(&is[0].text, &d), vec![("core/src/wifi.rs".to_string(), Some(42))]);
        let _ = std::fs::remove_dir_all(d);
    }
    #[test]
    fn diff_extraction() {
        let r = "Here you go:\n```diff\ndiff --git a/x.py b/x.py\n--- a/x.py\n+++ b/x.py\n@@ -1 +1 @@\n-a\n+b\n```\n";
        assert!(extract_diff(r).unwrap().starts_with("diff --git"));
        assert!(extract_diff("NO_FIX: needs hardware").is_none());
        assert!(extract_diff("just talk").is_none());
    }
}
