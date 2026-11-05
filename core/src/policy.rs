//! Update source policy: which repository updates come from, and the rules every fetch from it
//! must satisfy. A configured, policy-compliant source is a requirement — nothing is checked,
//! fetched or built without one.
//!
//! The default source (10.0) is the official **public** repository, fetched anonymously over
//! HTTPS (`auth = "public"`): no credential is needed, so none is used or stored, and every other
//! rule below (hardened git, no history rewrites, optional signatures) still applies. Set
//! `[updates] repo` to use your own fork or mirror instead.
//!
//! For a private repository (`require_private = true`, the default for anything but `public`):
//! * Authentication is a dedicated **read-only deploy key** (SSH, `auth = "deploy-key"`) or the
//!   user's git **credential helper** (HTTPS, `auth = "credential-helper"`, e.g. `gh auth
//!   setup-git` or libsecret). A token or password is never accepted in the URL or in
//!   config.toml, and never passed on a command line.
//! * SSH host keys are **pinned** in a known_hosts file of its own (GitHub's published keys are
//!   built in; any other host's key has to be confirmed by fingerprint). Unknown or changed host
//!   keys fail closed (`StrictHostKeyChecking=yes`).
//! * The repository must really be private: a credential-free probe that succeeds means it is
//!   public, and the update is refused.
//! * History may not be rewritten (the new head must descend from the installed commit) unless
//!   `allow_history_rewrite = true`.
//! * Optional `require_signed = true`: every new commit must carry a valid signature by a key in
//!   `update-allowed-signers` (git's SSH allowed-signers format).
//! * Git runs hardened and isolated: none of the system/global git config or GIT_CONFIG_* variables
//!   apply (so no `url.insteadOf` rewrite or proxy setting can redirect the source), only ssh/https
//!   (file only for a local mirror), no hooks, no submodules, fsck of every received object, TLS
//!   verification forced on, no interactive prompts. The credential helper, when used, is the one
//!   from your git config, passed explicitly.
//! * Fetching and building run as you; the administrator step only copies already built files.
use crate::st;
use crate::sys;
use std::path::{Path, PathBuf};

/// GitHub's published SSH host keys (docs.github.com, "GitHub's SSH key fingerprints").
pub const GITHUB_KNOWN_HOSTS: &str = "\
github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl
github.com ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=
github.com ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFXV1JHnsKgbLWNlhScqb2UmyRkQyytRLtL+38TGxkxCflmO+5Z8CSSNY7GidjMIZ7Q4zMjA2n1nGrlTDkzwDCsw+wqFPGQA179cnfGWOWRVruj16z6XyvxvjJwbz0wQZ75XK5tKSb7FNyeIEs4TT4jk+S4dhPeAUC5y+bDYirYgM4GC7uEnztnZyaVWQ7B381AK4Qdrwt51ZqExKbQpTUNn+EjqoTwvqNj4kqx5QUCI0ThS/YkOxJCXmPUWZbhjpCg56i+2aB6CmK2JGhn57K5mj0MNdBXA4/WnwH6XoPWJzK5Nyu2zB3nAZp+S5hpQs+p1vN1/wsjk=
";

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Auth {
    DeployKey,
    CredentialHelper,
    Local,
    /// a public repository over HTTPS: anonymous, no credential at all
    Public,
}

/// The official repository: used when `[updates] repo` is not set. A build can point at its own
/// (fork, mirror) with SHADOW_DEFAULT_UPDATE_REPO at compile time.
pub const DEFAULT_REPO: &str = match option_env!("SHADOW_DEFAULT_UPDATE_REPO") {
    Some(r) => r,
    None => "https://github.com/namidelacruz/shadow-tui-restricted.git",
};

#[derive(Clone, Debug)]
pub struct Policy {
    pub repo: String,
    pub branch: String,
    pub auth: Auth,
    pub require_private: bool,
    pub require_signed: bool,
    pub allow_history_rewrite: bool,
    pub key: PathBuf,
    pub known_hosts: PathBuf,
    pub signers: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Level {
    Pass,
    Warn,
    Fail,
}

#[derive(Clone, Debug)]
pub struct Check {
    pub level: Level,
    pub name: &'static str,
    pub detail: String,
}

fn c(level: Level, name: &'static str, detail: impl Into<String>) -> Check {
    Check { level, name, detail: detail.into() }
}

/// Where a URL points. `Local` = a path or file:// (a mirror on this machine).
#[derive(Clone, Debug, PartialEq)]
pub enum Remote {
    Ssh { user: String, host: String, port: Option<u16>, path: String },
    Https { host: String, path: String, userinfo: String },
    Local(String),
    Bad(String),
}

pub fn parse(url: &str) -> Remote {
    let u = url.trim();
    if u.is_empty() || u.contains(char::is_whitespace) || u.chars().any(|ch| ch.is_control()) {
        return Remote::Bad("empty or contains spaces".into());
    }
    if let Some(r) = u.strip_prefix("file://") {
        return Remote::Local(r.to_string());
    }
    if u.starts_with('/') {
        return Remote::Local(u.to_string());
    }
    if let Some(r) = u.strip_prefix("ssh://") {
        let (auth, path) = r.split_once('/').unwrap_or((r, ""));
        let (user, hp) = auth.rsplit_once('@').map(|(a, b)| (a.to_string(), b)).unwrap_or((String::new(), auth));
        let (host, port) = match hp.rsplit_once(':') {
            Some((h, p)) if !h.contains(']') || h.ends_with(']') => (h.trim_matches(['[', ']']).to_string(), p.parse().ok()),
            _ => (hp.to_string(), None),
        };
        return Remote::Ssh { user, host: host.to_lowercase(), port, path: path.to_string() };
    }
    if let Some(r) = u.strip_prefix("https://") {
        let (auth, path) = r.split_once('/').unwrap_or((r, ""));
        let (userinfo, host) = auth.rsplit_once('@').map(|(a, b)| (a.to_string(), b)).unwrap_or((String::new(), auth));
        return Remote::Https { host: host.to_lowercase(), path: path.to_string(), userinfo };
    }
    if u.starts_with("http://") || u.starts_with("git://") || u.contains("://") {
        return Remote::Bad("only ssh and https are allowed (http:// and git:// are unencrypted)".into());
    }
    // scp-like: user@host:path
    if let Some((auth, path)) = u.split_once(':') {
        if !auth.contains('/') {
            let (user, host) = auth.rsplit_once('@').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((String::new(), auth.to_string()));
            return Remote::Ssh { user, host: host.to_lowercase(), port: None, path: path.to_string() };
        }
    }
    Remote::Bad("not a repository URL".into())
}

impl Remote {
    pub fn host(&self) -> Option<&str> {
        match self {
            Remote::Ssh { host, .. } | Remote::Https { host, .. } => Some(host),
            _ => None,
        }
    }
    /// owner/repo for well-known forges
    pub fn slug(&self) -> Option<String> {
        let p = match self {
            Remote::Ssh { path, .. } | Remote::Https { path, .. } => path,
            _ => return None,
        };
        let p = p.trim_matches('/').trim_end_matches(".git");
        let parts: Vec<&str> = p.split('/').collect();
        (parts.len() == 2 && parts.iter().all(|x| !x.is_empty())).then(|| p.to_string())
    }
    /// The same repository over credential-free HTTPS, for the visibility probe
    pub fn anonymous_https(&self) -> Option<String> {
        let h = self.host()?;
        ["github.com", "gitlab.com", "codeberg.org", "bitbucket.org"].contains(&h).then(|| format!("https://{h}/{}.git", self.slug().unwrap_or_default()))
    }
}

/// Things that look like tokens or passwords: never allowed in the URL or in config.toml.
pub fn looks_secret(s: &str) -> bool {
    const PREFIXES: [&str; 8] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "glpat-", "x-access-token"];
    PREFIXES.iter().any(|p| s.contains(p))
}

fn state() -> PathBuf {
    st::state_dir()
}

pub fn load() -> Policy {
    let set = st::get("updates", "repo").unwrap_or_default().trim().to_string();
    let default = set.is_empty() && !DEFAULT_REPO.is_empty();
    let repo = if default { DEFAULT_REPO.to_string() } else { set };
    let branch = st::get("updates", "branch").filter(|b| !b.trim().is_empty()).unwrap_or_else(|| "main".into()).trim().to_string();
    let auth = match st::get("updates", "auth").as_deref().map(str::trim) {
        Some("credential-helper") => Auth::CredentialHelper,
        Some("deploy-key") => Auth::DeployKey,
        Some("public") => Auth::Public,
        _ if default => Auth::Public,
        _ => match parse(&repo) {
            Remote::Local(_) => Auth::Local,
            Remote::Https { .. } => Auth::CredentialHelper,
            _ => Auth::DeployKey,
        },
    };
    let auth = if matches!(parse(&repo), Remote::Local(_)) { Auth::Local } else { auth };
    Policy {
        repo,
        branch,
        auth,
        require_private: st::get_bool("updates", "require_private", auth != Auth::Public),
        require_signed: st::get_bool("updates", "require_signed", false),
        allow_history_rewrite: st::get_bool("updates", "allow_history_rewrite", false),
        key: state().join("update-deploy-key"),
        known_hosts: state().join("update-known-hosts"),
        signers: state().join("update-allowed-signers"),
    }
}

fn mode_ok(p: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(p).map_err(|_| format!("{} does not exist", p.display()))?;
    if m.mode() & 0o077 != 0 {
        return Err(format!("{} is readable by others (mode {:o}): chmod 600 it", p.display(), m.mode() & 0o777));
    }
    // SAFETY: getuid has no preconditions
    let me = unsafe { libc_getuid() };
    if m.uid() != me {
        return Err(format!("{} is not owned by you", p.display()));
    }
    Ok(())
}

extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

impl Policy {
    pub fn remote(&self) -> Remote {
        parse(&self.repo)
    }

    /// `-c` options and environment every git call for updates runs with.
    pub fn git_prefix(&self) -> (Vec<String>, Vec<(String, String)>) {
        let local = self.auth == Auth::Local;
        let mut a: Vec<String> = vec![
            "protocol.allow=never".into(),
            "protocol.https.allow=always".into(),
            format!("protocol.ssh.allow={}", if self.auth == Auth::Public { "never" } else { "always" }),
            format!("protocol.file.allow={}", if local { "always" } else { "never" }),
            "core.hooksPath=/dev/null".into(),
            "transfer.fsckObjects=true".into(),
            "fetch.recurseSubmodules=false".into(),
            "submodule.recurse=false".into(),
            "http.sslVerify=true".into(),
            "credential.interactive=never".into(),
            "core.askPass=".into(),
        ];
        if self.auth == Auth::CredentialHelper {
            // the global config is not read: carry over only its credential helper(s)
            a.push("credential.helper=".into());
            for h in user_helpers() {
                a.push(format!("credential.helper={h}"));
            }
        } else {
            // the deploy key is the only credential; don't let a helper hand out anything else
            a.push("credential.helper=".into());
        }
        let mut env = vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()), ("GIT_ASKPASS".into(), "/bin/false".into()), ("SSH_ASKPASS".into(), "/bin/false".into())];
        if matches!(self.remote(), Remote::Ssh { .. }) {
            let mut ssh = format!(
                "ssh -F /dev/null -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile={} -o GlobalKnownHostsFile=/dev/null -o UpdateHostKeys=no -o ConnectTimeout=20",
                sq(&self.known_hosts.to_string_lossy())
            );
            if self.auth == Auth::DeployKey {
                ssh += &format!(" -o IdentitiesOnly=yes -o IdentityAgent=none -i {}", sq(&self.key.to_string_lossy()));
            }
            env.push(("GIT_SSH_COMMAND".into(), ssh));
        }
        let mut cfg = vec![];
        for x in a {
            cfg.push("-c".to_string());
            cfg.push(x);
        }
        (cfg, env)
    }

    /// git with the policy applied
    pub fn git(&self, dir: Option<&Path>, args: &[&str]) -> (i32, String, String) {
        let (cfg, env) = self.git_prefix();
        let mut c = std::process::Command::new("git");
        isolate(&mut c);
        if let Some(d) = dir {
            c.arg("-C").arg(d);
        }
        c.args(&cfg).args(args).stdin(std::process::Stdio::null());
        for (k, v) in env {
            c.env(k, v);
        }
        match c.output() {
            Ok(o) => (o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned()),
            Err(e) => (127, String::new(), format!("git: {e}")),
        }
    }

    /// The rules that can be checked without the network.
    pub fn offline(&self) -> Vec<Check> {
        let mut v = vec![];
        if self.repo.is_empty() {
            v.push(c(Level::Fail, "source", "no update repository is set up — run: shadow update setup"));
            return v;
        }
        let r = self.remote();
        match &r {
            Remote::Bad(e) => v.push(c(Level::Fail, "source", format!("{}: {e}", self.repo))),
            Remote::Local(p) => v.push(c(Level::Pass, "source", format!("local mirror {p} (no network, no credentials)"))),
            _ => v.push(c(Level::Pass, "source", format!("{} ({})", self.repo, self.branch))),
        }
        if self.branch.is_empty() || self.branch.len() > 100 || self.branch.contains("..") || self.branch.starts_with('-') || !self.branch.chars().all(|ch| ch.is_ascii_alphanumeric() || "._/-".contains(ch)) {
            v.push(c(Level::Fail, "branch", format!("invalid branch name '{}'", self.branch)));
        }
        if sys::which("git").is_none() {
            v.push(c(Level::Fail, "git", "git is not installed"));
        }
        // no secrets in the URL or the config file
        if let Remote::Https { userinfo, .. } = &r {
            if !userinfo.is_empty() {
                v.push(c(Level::Fail, "credentials", "the URL contains a user name or token: remove it — use a deploy key or a credential helper"));
            }
        }
        let cfg = std::fs::read_to_string(st::config_file()).unwrap_or_default();
        if looks_secret(&self.repo) || cfg.lines().any(|l| !l.trim_start().starts_with('#') && looks_secret(l)) {
            v.push(c(Level::Fail, "credentials", "config.toml contains what looks like an access token: delete it and revoke it"));
        } else if !matches!(&r, Remote::Https { userinfo, .. } if !userinfo.is_empty()) {
            v.push(c(Level::Pass, "credentials", "none stored in the URL or config.toml"));
        }
        match (self.auth, &r) {
            (Auth::DeployKey, Remote::Ssh { host, .. }) => {
                match mode_ok(&self.key) {
                    Ok(()) if self.key.with_extension("pub").is_file() => v.push(c(Level::Pass, "auth", format!("deploy key {} (add it read-only)", self.key.display()))),
                    Ok(()) => v.push(c(Level::Fail, "auth", format!("{}.pub is missing", self.key.display()))),
                    Err(e) => v.push(c(Level::Fail, "auth", format!("deploy key: {e} — run: shadow update setup"))),
                }
                let kh = std::fs::read_to_string(&self.known_hosts).unwrap_or_default();
                let pinned: Vec<&str> = kh.lines().filter(|l| l.split_whitespace().next().is_some_and(|h| h.split(',').any(|x| x == host || x.trim_matches(['[', ']']).split(':').next() == Some(host)))).collect();
                if pinned.is_empty() {
                    v.push(c(Level::Fail, "host key", format!("no pinned host key for {host} — run: shadow update setup")));
                } else if host == "github.com" && !pinned.iter().all(|l| GITHUB_KNOWN_HOSTS.lines().any(|g| g.split_whitespace().nth(2) == l.split_whitespace().nth(2))) {
                    v.push(c(Level::Fail, "host key", "a pinned github.com key is not one of GitHub's published keys"));
                } else {
                    v.push(c(Level::Pass, "host key", format!("{host} pinned ({} key(s))", pinned.len())));
                }
            }
            (Auth::DeployKey, Remote::Https { .. }) => v.push(c(Level::Fail, "auth", "a deploy key needs an ssh URL (git@host:owner/repo.git)")),
            (Auth::CredentialHelper, Remote::Https { .. }) => {
                let all = {
                    let mut c = std::process::Command::new("git");
                    for (k, _) in std::env::vars_os() {
                        if k.to_string_lossy().starts_with("GIT_CONFIG") {
                            c.env_remove(&k);
                        }
                    }
                    c.args(["config", "--global", "--get-all", "credential.helper"]).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
                };
                let helper = all.lines().filter(|h| h.trim() != "store").collect::<Vec<_>>().join("\n");
                if all.lines().any(|h| h.trim() == "store") && helper.trim().is_empty() {
                    v.push(c(Level::Fail, "auth", "credential.helper=store keeps the token in plain text (~/.git-credentials): use gh or libsecret"));
                } else if helper.trim().is_empty() {
                    v.push(c(if self.require_private { Level::Fail } else { Level::Warn }, "auth", "no git credential helper: run `gh auth setup-git`, or store a read-only token with git-credential-libsecret"));
                } else {
                    v.push(c(Level::Pass, "auth", format!("credential helper: {}", helper.lines().next().unwrap_or("").trim())));
                }
            }
            (Auth::Public, Remote::Https { .. }) if self.require_private => v.push(c(Level::Fail, "auth", "auth = \"public\" but require_private = true: pick one")),
            (Auth::Public, Remote::Https { .. }) => v.push(c(Level::Pass, "auth", "public repository: fetched anonymously, no credential used or stored")),
            (Auth::Public, _) => v.push(c(Level::Fail, "auth", "auth = \"public\" needs an https:// URL")),
            (Auth::CredentialHelper, Remote::Ssh { .. }) => v.push(c(Level::Warn, "auth", "ssh URL with auth = credential-helper: your ssh agent/keys are not used; set auth = \"deploy-key\"")),
            _ => {}
        }
        if self.require_signed {
            match std::fs::read_to_string(&self.signers) {
                Ok(t) if t.lines().any(|l| !l.trim().is_empty() && !l.starts_with('#')) => v.push(c(Level::Pass, "signatures", format!("required; signers in {}", self.signers.display()))),
                _ => v.push(c(Level::Fail, "signatures", format!("require_signed is on but {} lists no signer", self.signers.display()))),
            }
        }
        v
    }

    /// Network checks: the source is reachable with the configured credential, and (if required)
    /// not readable without one.
    pub fn online(&self) -> Vec<Check> {
        let mut v = vec![];
        let (code, out, err) = self.git(None, &["ls-remote", "--heads", &self.repo, &format!("refs/heads/{}", self.branch)]);
        if code != 0 {
            // the most telling line: ssh's own reason beats git's generic "could not read"
            let pick = |f: &dyn Fn(&str) -> bool| err.lines().find(|l| f(l)).map(|l| l.trim().to_string());
            let e = pick(&|l| l.contains("denied") || l.contains("Host key") || l.contains("HOST IDENTIFICATION") || l.contains("not found") || l.contains("ERROR"))
                .or_else(|| pick(&|l| l.starts_with("ssh:") || l.contains("Could not resolve") || l.contains("timed out")))
                .or_else(|| pick(&|l| l.contains("fatal")))
                .unwrap_or_else(|| err.lines().next().unwrap_or("").trim().to_string());
            let hint = if e.contains("Permission denied") || e.contains("Repository not found") || e.contains("not found") {
                " — is the deploy key added to the repository?"
            } else if e.contains("Host key verification failed") || e.contains("REMOTE HOST IDENTIFICATION") {
                " — the server's host key does not match the pinned one: do NOT continue until you know why"
            } else {
                ""
            };
            v.push(c(Level::Fail, "access", format!("cannot read the repository: {e}{hint}")));
        } else if out.trim().is_empty() {
            v.push(c(Level::Fail, "access", format!("branch '{}' does not exist", self.branch)));
        } else {
            v.push(c(Level::Pass, "access", if self.auth == Auth::Public { "the repository is readable anonymously" } else { "the repository is readable with the configured credential" }));
        }
        if self.auth == Auth::Local {
            return v;
        }
        if self.auth == Auth::Public {
            // the access check above already ran without any credential
            v.push(c(Level::Pass, "private", "public by choice: readable anonymously, as configured"));
            return v;
        }
        match self.remote().anonymous_https() {
            Some(anon) => {
                let mut cmd = std::process::Command::new("git");
                isolate(&mut cmd);
                cmd.args(["-c", "credential.helper=", "-c", "core.askPass=", "-c", "http.sslVerify=true", "ls-remote", "--heads", &anon]).env("GIT_TERMINAL_PROMPT", "0").env("GIT_ASKPASS", "/bin/false").env("GIT_CONFIG_NOSYSTEM", "1").env("HOME", "/nonexistent").env("XDG_CONFIG_HOME", "/nonexistent").stdin(std::process::Stdio::null());
                match cmd.output() {
                    Ok(o) if o.status.success() => v.push(c(if self.require_private { Level::Fail } else { Level::Warn }, "private", format!("{anon} is readable by anyone: the repository is PUBLIC{}", if self.require_private { " — make it private (or set require_private = false)" } else { "" }))),
                    Ok(o) => {
                        let e = String::from_utf8_lossy(&o.stderr);
                        if e.contains("Could not resolve") || e.contains("unable to access") && !e.contains("40") {
                            v.push(c(Level::Warn, "private", "could not reach the server anonymously to confirm it is private"));
                        } else {
                            v.push(c(Level::Pass, "private", "not readable without credentials"));
                        }
                    }
                    Err(_) => v.push(c(Level::Warn, "private", "could not run the visibility probe")),
                }
            }
            None => v.push(c(Level::Warn, "private", format!("{} is not a known forge: visibility not verified", self.remote().host().unwrap_or("?")))),
        }
        v
    }

    /// The history rules for an update from `base` to `head` (in `dir`).
    pub fn verify_history(&self, dir: &Path, base: &str, head: &str, base_known: bool) -> Result<(), String> {
        if base_known && base != head && !self.allow_history_rewrite && self.git(Some(dir), &["merge-base", "--is-ancestor", base, head]).0 != 0 {
            return Err(format!("the branch history was rewritten (the installed commit {} is not an ancestor of {}): refused — set allow_history_rewrite = true only if you did that yourself", &base[..8.min(base.len())], &head[..8.min(head.len())]));
        }
        if self.require_signed {
            let range = if base_known && base != head { format!("{base}..{head}") } else { head.to_string() };
            let list = self.git(Some(dir), &["rev-list", "--max-count=500", &range]).1;
            let signers = self.signers.to_string_lossy().into_owned();
            for commit in list.lines().filter(|l| !l.is_empty()) {
                let (c, _, e) = self.git(Some(dir), &["-c", &format!("gpg.ssh.allowedSignersFile={signers}"), "verify-commit", commit]);
                if c != 0 {
                    return Err(format!("commit {} is not signed by an allowed signer: refused ({})", &commit[..8.min(commit.len())], e.lines().last().unwrap_or("").trim()));
                }
            }
        }
        Ok(())
    }
}

/// Fail if any rule fails (online too, when `net`). Returns the warnings.
pub fn enforce(p: &Policy, net: bool) -> Result<Vec<Check>, String> {
    let mut all = p.offline();
    if !all.iter().any(|x| x.level == Level::Fail) && net {
        all.extend(p.online());
    }
    if let Some(f) = all.iter().find(|x| x.level == Level::Fail) {
        return Err(format!("update source policy: {} — {}", f.name, f.detail));
    }
    Ok(all.into_iter().filter(|x| x.level == Level::Warn).collect())
}

/// No inherited git configuration: system and global files and every GIT_* override are dropped.
fn isolate(c: &mut std::process::Command) {
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if k.starts_with("GIT_CONFIG") || ["GIT_SSH", "GIT_SSH_COMMAND", "GIT_PROXY_COMMAND", "GIT_ASKPASS", "GIT_EXEC_PATH", "GIT_DIR", "GIT_WORK_TREE", "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_OBJECT_DIRECTORY", "GIT_TEMPLATE_DIR"].contains(&k.as_str()) {
            c.env_remove(&k);
        }
    }
    c.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null");
}

/// The credential helper(s) from your own git config (read without the environment overrides)
fn user_helpers() -> Vec<String> {
    let mut c = std::process::Command::new("git");
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_CONFIG") {
            c.env_remove(&k);
        }
    }
    c.args(["config", "--global", "--get-all", "credential.helper"]);
    c.output().map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && l != "store").collect()).unwrap_or_default()
}

fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn print(checks: &[Check], porcelain: bool) {
    for x in checks {
        let l = match x.level {
            Level::Pass => "pass",
            Level::Warn => "warn",
            Level::Fail => "fail",
        };
        if porcelain {
            println!("{l} {} {}", x.name, x.detail);
        } else {
            let mark = match x.level {
                Level::Pass => "\x1b[32m✓\x1b[0m",
                Level::Warn => "\x1b[33m!\x1b[0m",
                Level::Fail => "\x1b[31m✗\x1b[0m",
            };
            println!("  {mark} {:<12} {}", x.name, x.detail);
        }
    }
}

// ── setup wizard ─────────────────────────────────────────────────────────────────────────────────

fn ask(prompt: &str, default: &str, yes: bool) -> String {
    use std::io::Write;
    if yes || !crate::updater::tty() {
        return default.to_string();
    }
    print!("{prompt}{}: ", if default.is_empty() { String::new() } else { format!(" [{default}]") });
    let _ = std::io::stdout().flush();
    let mut l = String::new();
    let _ = std::io::stdin().read_line(&mut l);
    let l = l.trim();
    if l.is_empty() { default.to_string() } else { l.to_string() }
}

fn yesno(prompt: &str, default: bool, yes: bool) -> bool {
    let a = ask(&format!("{prompt} ({})", if default { "Y/n" } else { "y/N" }), "", yes);
    if a.is_empty() { default } else { a.to_lowercase().starts_with('y') }
}

/// `https://github.com/o/r(.git)` → `git@github.com:o/r.git` (deploy keys are SSH)
pub fn to_ssh(url: &str) -> String {
    match parse(url) {
        Remote::Https { host, path, userinfo } if userinfo.is_empty() && ["github.com", "gitlab.com", "codeberg.org", "bitbucket.org"].contains(&host.as_str()) => {
            format!("git@{host}:{}.git", path.trim_matches('/').trim_end_matches(".git"))
        }
        _ => url.to_string(),
    }
}

fn fingerprints(lines: &str) -> Vec<String> {
    let mut c = std::process::Command::new("ssh-keygen");
    c.args(["-l", "-f", "-"]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    let Ok(mut ch) = c.spawn() else { return vec![] };
    if let Some(mut i) = ch.stdin.take() {
        use std::io::Write;
        let _ = i.write_all(lines.as_bytes());
    }
    ch.wait_with_output().map(|o| String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect()).unwrap_or_default()
}

/// A credential-free `ls-remote` succeeds: anyone can read the repository.
pub fn anon_readable(anon: &str) -> bool {
    let mut cmd = std::process::Command::new("git");
    isolate(&mut cmd);
    cmd.args(["-c", "credential.helper=", "-c", "core.askPass=", "-c", "http.sslVerify=true", "-c", "protocol.allow=never", "-c", "protocol.https.allow=always", "ls-remote", "--heads", anon])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/false")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", "/nonexistent")
        .env("XDG_CONFIG_HOME", "/nonexistent")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.status().is_ok_and(|s| s.success())
}

/// `shadow update setup [URL] [--branch B] [--public | --https | --deploy-key] [--host-fingerprint SHA256:…] [--yes]`
pub fn setup(args: &[&str]) -> i32 {
    let yes = args.contains(&"--yes") || args.contains(&"-y");
    let val = |k: &str| args.iter().position(|a| *a == k).and_then(|i| args.get(i + 1)).map(|s| s.to_string());
    let pos: Vec<&str> = {
        let mut v = vec![];
        let mut skip = false;
        for a in args {
            if skip {
                skip = false;
                continue;
            }
            if ["--branch", "--host-fingerprint"].contains(a) {
                skip = true;
                continue;
            }
            if !a.starts_with('-') {
                v.push(*a);
            }
        }
        v
    };
    let cur = load();
    println!("Shadow TUI — update source setup\n");
    println!("Updates come only from this repository. For a private repository Shadow uses a read-only");
    println!("deploy key (recommended) or your git credential helper; tokens are never stored by Shadow.\n");
    let mut url = pos.first().map(|s| s.to_string()).unwrap_or_else(|| ask("Repository URL (git@github.com:owner/repo.git)", &cur.repo, yes));
    if url.is_empty() {
        eprintln!("setup: a repository URL is required");
        return 1;
    }
    if looks_secret(&url) || matches!(parse(&url), Remote::Https { ref userinfo, .. } if !userinfo.is_empty()) {
        eprintln!("setup: the URL contains credentials. Remove them (and revoke that token): Shadow never stores tokens.");
        return 1;
    }
    let https = args.contains(&"--https");
    // a public repository needs no key and no token: fetched anonymously over HTTPS
    let public = args.contains(&"--public")
        || (!https && !args.contains(&"--deploy-key") && parse(&url).anonymous_https().is_some_and(|a| {
            println!("  checking whether the repository is public…");
            anon_readable(&a)
        }));
    if public {
        if let Some(a) = parse(&url).anonymous_https() {
            url = a;
        }
        println!("  public repository: updates are fetched anonymously over HTTPS (no key, no token)");
    }
    if !https && !public {
        let s = to_ssh(&url);
        if s != url {
            println!("  using the SSH form for a deploy key: {s}");
            url = s;
        }
    }
    let r = parse(&url);
    if let Remote::Bad(e) = &r {
        eprintln!("setup: {url}: {e}");
        return 1;
    }
    let branch = val("--branch").unwrap_or_else(|| ask("Branch", &cur.branch, yes));
    let auth = match &r {
        Remote::Local(_) => "local",
        Remote::Https { .. } if public => "public",
        Remote::Https { .. } => "credential-helper",
        _ => "deploy-key",
    };
    let p = Policy { repo: url.clone(), branch: branch.clone(), auth: match auth { "local" => Auth::Local, "public" => Auth::Public, "credential-helper" => Auth::CredentialHelper, _ => Auth::DeployKey }, ..cur.clone() };
    let _ = std::fs::create_dir_all(state());
    if let Remote::Ssh { host, port, .. } = &r {
        // 1. deploy key
        if !p.key.is_file() {
            let host_name = std::fs::read_to_string("/etc/hostname").unwrap_or_default().trim().to_string();
            let comment = format!("shadow-update@{host_name}");
            let (c, _, e) = sys::run(&["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f", &p.key.to_string_lossy()]);
            if c != 0 {
                eprintln!("setup: ssh-keygen failed: {}", e.trim());
                return 1;
            }
            println!("  created a dedicated deploy key {}", p.key.display());
        }
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p.key, std::fs::Permissions::from_mode(0o600));
        let pubkey = std::fs::read_to_string(p.key.with_extension("pub")).unwrap_or_default();
        // 2. host key pinning
        let mut kh = std::fs::read_to_string(&p.known_hosts).unwrap_or_default();
        let known = kh.lines().any(|l| l.split_whitespace().next().is_some_and(|h| h.split(',').any(|x| x == host)));
        if host == "github.com" {
            if !known {
                kh.push_str(GITHUB_KNOWN_HOSTS);
                println!("  pinned GitHub's published host keys");
            }
        } else if !known {
            let mut a = vec!["ssh-keyscan".to_string(), "-T".into(), "10".into()];
            if let Some(pt) = port {
                a.extend(["-p".into(), pt.to_string()]);
            }
            a.push(host.clone());
            let av: Vec<&str> = a.iter().map(String::as_str).collect();
            let scanned = sys::run(&av).1;
            let fps = fingerprints(&scanned);
            if fps.is_empty() {
                eprintln!("setup: could not read {host}'s host key (offline?)");
                return 1;
            }
            println!("  {host} presents these host keys:");
            for f in &fps {
                println!("     {f}");
            }
            let trusted = match val("--host-fingerprint") {
                Some(want) => fps.iter().any(|f| f.split_whitespace().nth(1) == Some(want.as_str())),
                None => !yes && yesno("  Do these match the fingerprints your server administrator published?", false, false),
            };
            if !trusted {
                eprintln!("setup: host key not confirmed — nothing pinned. Re-run with --host-fingerprint SHA256:… from a trusted source.");
                return 1;
            }
            kh.push_str(&scanned);
        }
        if crate::updater::write_private(&p.known_hosts, &kh).is_err() {
            eprintln!("setup: cannot write {}", p.known_hosts.display());
            return 1;
        }
        // 3. register the deploy key read-only
        println!("\n  Add this key to the repository as a READ-ONLY deploy key:");
        println!("     {}", pubkey.trim());
        let slug = r.slug().unwrap_or_default();
        let gh_ok = host == "github.com" && sys::which("gh").is_some() && sys::run(&["gh", "auth", "status"]).0 == 0;
        let mut added = false;
        if gh_ok && !slug.is_empty() && yesno(&format!("  Add it to {slug} now with gh (read-only)?"), true, yes) {
            let title = format!("Shadow update ({})", std::fs::read_to_string("/etc/hostname").unwrap_or_default().trim());
            // gh adds deploy keys read-only unless --allow-write is given
            let (c, o, e) = sys::run(&["gh", "repo", "deploy-key", "add", &p.key.with_extension("pub").to_string_lossy(), "--repo", &slug, "--title", &title]);
            if c == 0 {
                added = true;
                println!("  added (read-only) to {slug}");
            } else if (o + &e).contains("already in use") {
                added = true;
                println!("  this key is already registered");
            } else {
                println!("  gh could not add it: {}", e.trim());
            }
        }
        if !added {
            if host == "github.com" {
                println!("     GitHub → {} → Settings → Deploy keys → Add deploy key", if slug.is_empty() { "the repository".into() } else { slug.clone() });
                println!("     paste the key above and leave \"Allow write access\" UNCHECKED.");
            } else {
                println!("     add it in your forge's repository settings as a deploy key without write access.");
            }
            if !yes && crate::updater::tty() {
                ask("  Press Enter once it is added", "", false);
            }
        }
    } else if let Remote::Https { .. } = &r {
        println!("  HTTPS: credentials come from your git credential helper (never from Shadow).");
        println!("  Use a fine-grained token limited to this repository with read-only \"Contents\" access,");
        println!("  e.g.  gh auth login && gh auth setup-git   — or git-credential-libsecret.");
    }
    // 4. save (literal values only; never a secret)
    let ok = st::set("updates", "repo", &st::q(&url)).and_then(|_| st::set("updates", "branch", &st::q(&branch))).and_then(|_| st::set("updates", "auth", &st::q(auth)));
    let ok = ok.and_then(|_| {
        if auth == "public" {
            st::set("updates", "require_private", "false")
        } else if st::get("updates", "require_private").is_none() {
            st::set("updates", "require_private", "true")
        } else {
            Ok(())
        }
    });
    if ok.is_err() {
        eprintln!("setup: cannot write {}", st::config_file().display());
        return 1;
    }
    println!("\nChecking the policy:");
    let p = load();
    let mut checks = p.offline();
    if !checks.iter().any(|x| x.level == Level::Fail) {
        checks.extend(p.online());
    }
    print(&checks, false);
    if checks.iter().any(|x| x.level == Level::Fail) {
        println!("\nNot ready yet: fix the item(s) marked ✗, then run  shadow update policy  again.");
        return 3;
    }
    println!("\nReady: updates come from {} ({}).", p.repo, p.branch);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_repositories_need_no_credential_and_no_ssh() {
        let p = Policy { repo: DEFAULT_REPO.into(), branch: "main".into(), auth: Auth::Public, require_private: false, require_signed: false, allow_history_rewrite: false, key: "/k".into(), known_hosts: "/kh".into(), signers: "/s".into() };
        let off = p.offline();
        let auth = off.iter().find(|c| c.name == "auth").unwrap();
        assert_eq!(auth.level, Level::Pass, "{}", auth.detail);
        let (cfg, env) = p.git_prefix();
        assert!(cfg.iter().any(|x| x == "protocol.ssh.allow=never"));
        assert!(cfg.iter().any(|x| x == "credential.helper="));
        assert!(!cfg.iter().any(|x| x.starts_with("credential.helper=") && x != "credential.helper="));
        assert!(env.iter().all(|(k, _)| k != "GIT_SSH_COMMAND"));
        // contradictory settings fail closed
        let both = Policy { require_private: true, ..p.clone() };
        assert_eq!(both.offline().iter().find(|c| c.name == "auth").unwrap().level, Level::Fail);
        // public needs https
        let ssh = Policy { repo: "git@github.com:o/r.git".into(), ..p };
        assert_eq!(ssh.offline().iter().find(|c| c.name == "auth").unwrap().level, Level::Fail);
        assert!(DEFAULT_REPO.starts_with("https://") && !looks_secret(DEFAULT_REPO));
    }

    #[test]
    fn urls() {
        assert!(matches!(parse("git@github.com:namidelacruz/shadow-tui-restricted.git"), Remote::Ssh { ref host, .. } if host == "github.com"));
        assert_eq!(parse("git@github.com:o/r.git").slug().as_deref(), Some("o/r"));
        assert!(matches!(parse("ssh://git@git.example.org:2222/o/r.git"), Remote::Ssh { port: Some(2222), ref host, .. } if host == "git.example.org"));
        assert!(matches!(parse("https://x-access-token:abc@github.com/o/r"), Remote::Https { ref userinfo, .. } if !userinfo.is_empty()));
        assert!(matches!(parse("http://github.com/o/r"), Remote::Bad(_)));
        assert!(matches!(parse("git://github.com/o/r"), Remote::Bad(_)));
        assert!(matches!(parse("/srv/mirror.git"), Remote::Local(_)));
        assert!(matches!(parse("file:///srv/mirror.git"), Remote::Local(_)));
        assert_eq!(to_ssh("https://github.com/o/r"), "git@github.com:o/r.git");
        assert_eq!(to_ssh("https://github.com/o/r.git"), "git@github.com:o/r.git");
        assert_eq!(parse("git@github.com:o/r.git").anonymous_https().as_deref(), Some("https://github.com/o/r.git"));
    }

    #[test]
    fn secrets() {
        assert!(looks_secret("token = \"ghp_abcdef\""));
        assert!(looks_secret("https://github_pat_11AA@github.com/o/r"));
        assert!(!looks_secret("repo = \"git@github.com:o/r.git\""));
    }

    #[test]
    fn hardened_git() {
        let p = Policy { repo: "git@github.com:o/r.git".into(), branch: "main".into(), auth: Auth::DeployKey, require_private: true, require_signed: false, allow_history_rewrite: false, key: "/k".into(), known_hosts: "/kh".into(), signers: "/s".into() };
        let (cfg, env) = p.git_prefix();
        let cfg = cfg.join(" ");
        assert!(cfg.contains("protocol.file.allow=never") && cfg.contains("core.hooksPath=/dev/null") && cfg.contains("transfer.fsckObjects=true") && cfg.contains("credential.helper="));
        let ssh = &env.iter().find(|(k, _)| k == "GIT_SSH_COMMAND").unwrap().1;
        assert!(ssh.contains("StrictHostKeyChecking=yes") && ssh.contains("IdentitiesOnly=yes") && ssh.contains("-i '/k'") && ssh.contains("UserKnownHostsFile='/kh'"));
        assert!(env.iter().any(|(k, v)| k == "GIT_TERMINAL_PROMPT" && v == "0"));
    }

    fn sh(dir: &Path, a: &[&str]) -> String {
        let o = std::process::Command::new(a[0]).args(&a[1..]).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").env_remove("GIT_CONFIG_COUNT").output().unwrap();
        assert!(o.status.success(), "{a:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    #[test]
    fn history_rewrites_and_unsigned_commits_are_refused() {
        if sys::which("ssh-keygen").is_none() {
            return;
        }
        let t = std::env::temp_dir().join(format!("pol-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let g = |a: &[&str]| {
            let mut v = vec!["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"];
            v.extend_from_slice(a);
            sh(&t, &v)
        };
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(t.join("a"), "1").unwrap();
        g(&["add", "a"]);
        g(&["commit", "-qm", "base"]);
        let base = g(&["rev-parse", "HEAD"]);
        std::fs::write(t.join("a"), "2").unwrap();
        g(&["commit", "-qam", "next"]);
        let head = g(&["rev-parse", "HEAD"]);
        let mut p = Policy { repo: t.to_string_lossy().into(), branch: "main".into(), auth: Auth::Local, require_private: true, require_signed: false, allow_history_rewrite: false, key: t.join("k"), known_hosts: t.join("kh"), signers: t.join("signers") };
        assert!(p.verify_history(&t, &base, &head, true).is_ok());
        // a rewritten branch: the installed commit is no longer an ancestor
        g(&["reset", "-q", "--hard", "HEAD~1"]);
        g(&["commit", "-q", "--amend", "-m", "rewritten"]);
        let forced = g(&["rev-parse", "HEAD"]);
        assert!(p.verify_history(&t, &base, &forced, true).unwrap_err().contains("rewritten"));
        p.allow_history_rewrite = true;
        assert!(p.verify_history(&t, &base, &forced, true).is_ok());
        p.allow_history_rewrite = false;
        // signatures: unsigned refused, signed by an allowed key accepted
        p.require_signed = true;
        let key = t.join("signkey");
        sh(&t, &["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", &key.to_string_lossy()]);
        let pubk = std::fs::read_to_string(key.with_extension("pub")).unwrap();
        std::fs::write(&p.signers, format!("t@t {}", pubk)).unwrap();
        assert!(p.verify_history(&t, &base, &head, true).unwrap_err().contains("not signed"));
        g(&["checkout", "-q", &head]);
        std::fs::write(t.join("a"), "3").unwrap();
        g(&["-c", "gpg.format=ssh", "-c", &format!("user.signingkey={}", key.to_string_lossy()), "commit", "-q", "-S", "-am", "signed"]);
        let signed = g(&["rev-parse", "HEAD"]);
        assert!(p.verify_history(&t, &head, &signed, true).is_ok(), "{:?}", p.verify_history(&t, &head, &signed, true));
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn inherited_git_config_cannot_redirect_the_source() {
        // e.g. GIT_CONFIG_* injecting url.<x>.insteadOf: removed for every update git call
        let mut c = std::process::Command::new("git");
        isolate(&mut c);
        let envs: Vec<_> = c.get_envs().map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(|v| v.to_string_lossy().into_owned()))).collect();
        assert!(envs.contains(&("GIT_CONFIG_GLOBAL".into(), Some("/dev/null".into()))));
        assert!(envs.contains(&("GIT_CONFIG_NOSYSTEM".into(), Some("1".into()))));
        for (k, _) in std::env::vars_os() {
            let k = k.to_string_lossy().into_owned();
            if k.starts_with("GIT_CONFIG_KEY") {
                assert!(envs.contains(&(k, None)), "inherited override removed");
            }
        }
    }

    #[test]
    fn github_pins_are_the_published_fingerprints() {
        if sys::which("ssh-keygen").is_none() {
            return;
        }
        let f = fingerprints(GITHUB_KNOWN_HOSTS).join("\n");
        for want in ["SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU", "SHA256:p2QAMXNIC1TJYWeIOttrVc98/R1BUFWu3/LiyKgUfQM", "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s"] {
            assert!(f.contains(want), "{want} missing in {f}");
        }
    }
}
