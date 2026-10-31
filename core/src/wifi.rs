//! Wi-Fi with self-correction (NetworkManager via nmcli) — port of wifi.py.
//!
//! Campus presets
//!   ucalgary : UC-Secure (new) + AirUC-Secure (old name, still live in buildings not yet
//!              upgraded during UCalgary's 2026 Wi-Fi transition). PEAP/MSCHAPv2, username =
//!              first half of the email, server validated against the system CAs + ucalgary.ca.
//!   eduroam  : PEAP/MSCHAPv2 with the FULL email as username, realm-checked.
//!
//! After a failed connect the NetworkManager / wpa_supplicant log is classified, the next fix
//! for that failure is applied and the connect retried. Fixes that worked are remembered per
//! profile. Password failures are never retried more than once (no account lock-outs).
use crate::st;
use crate::json::{self, Value};
use crate::rx;
use crate::sys;
use std::collections::HashMap;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub type Cb<'a> = Option<&'a dyn Fn(&str)>;

const CA_BUNDLE: &str = "/etc/ssl/certs/ca-certificates.crt";
/// Shadow's own trust store (verified roots, installed read-only) when present, else the system's
pub fn ca_bundle() -> String {
    let own = "/etc/shadow-tui/certs/ca-bundle.pem";
    if Path::new(own).exists() { own.into() } else { CA_BUNDLE.into() }
}
pub const PREFIX: &str = "SN ";
const WATCHER: &str = "shadow-wifi-device.service";
const DEVICE_LEARNED: &str = "device";

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.to_string())
}
fn nmcli() -> String { env_or("SHADOWNET_NMCLI", "nmcli") }
fn journalctl() -> String { env_or("SHADOWNET_JOURNALCTL", "journalctl") }
fn helper() -> String { env_or("SHADOWNET_WIFI_HELPER", "/opt/shadow-tui/lib/wifi-helper") }
fn dev_helper() -> String { env_or("SHADOWNET_WIFI_DEVICE_HELPER", "/opt/shadow-tui/lib/wifi-device-helper") }
fn systemctl() -> String { env_or("SHADOWNET_SYSTEMCTL", "systemctl") }
fn sysfs() -> PathBuf { PathBuf::from(env_or("SHADOWNET_SYSFS", "/sys")) }
fn etc() -> PathBuf { PathBuf::from(env_or("SHADOWNET_ETC", "/etc")) }
fn device_log() -> PathBuf { PathBuf::from(env_or("SHADOWNET_RUN", "/run")).join("shadow-network/wifi-device.log") }
fn state() -> PathBuf { st::local_state() }
fn learned_file() -> PathBuf { state().join("wifi-fixes.json") }
fn wifi_log() -> PathBuf { state().join("wifi.log") }

#[allow(dead_code)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    ssids: &'static [(&'static str, u32)],
    identity: &'static str,
    domain: &'static str,
    realm: &'static str,
}
pub const PRESETS: [Preset; 2] = [
    Preset { id: "ucalgary", label: "UCalgary — UC-Secure + AirUC-Secure", ssids: &[("UC-Secure", 30), ("AirUC-Secure", 20)], identity: "{user}", domain: "ucalgary.ca", realm: "ucalgary.ca" },
    Preset { id: "eduroam", label: "eduroam", ssids: &[("eduroam", 10)], identity: "{user}@{realm}", domain: "{realm}", realm: "ucalgary.ca" },
];

// ------------------------------------------------------------------ plumbing
fn tz_offset() -> i64 {
    static OFF: OnceLock<i64> = OnceLock::new();
    *OFF.get_or_init(|| {
        let z = sys::run(&["date", "+%z"]).1;
        let (sign, d) = if let Some(r) = z.strip_prefix('-') { (-1, r) } else { (1, z.trim_start_matches('+')) };
        let h: i64 = d.get(0..2).and_then(|x| x.parse().ok()).unwrap_or(0);
        let m: i64 = d.get(2..4).and_then(|x| x.parse().ok()).unwrap_or(0);
        sign * (h * 3600 + m * 60)
    })
}

/// (date "YYYY-MM-DD", time "HH:MM:SS") in local time
fn stamp() -> (String, String) {
    let t = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) + tz_offset();
    let (days, secs) = (t.div_euclid(86400), t.rem_euclid(86400));
    // civil_from_days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (format!("{y:04}-{m:02}-{d:02}"), format!("{:02}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60))
}

pub fn log(msg: &str, cb: Cb) {
    let (d, t) = stamp();
    let line = format!("{t} {msg}");
    let _ = std::fs::create_dir_all(state());
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(wifi_log()) {
        let _ = writeln!(f, "{d} {line}");
    }
    if let Some(cb) = cb {
        cb(&line);
    }
}

/// Run with a timeout; (code, stdout, stderr) untrimmed. 127 = not found, 124 = timed out.
fn exec(cmd: &str, args: &[&str], input: Option<&str>, timeout: u64, lc_all: bool) -> (i32, String, String) {
    let mut c = Command::new(cmd);
    c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if lc_all {
        c.env("LC_ALL", "C");
    }
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let Ok(mut ch) = c.spawn() else { return (127, String::new(), format!("{cmd}: not found")) };
    if let (Some(t), Some(mut si)) = (input, ch.stdin.take()) {
        use std::io::Write;
        let _ = si.write_all(t.as_bytes());
    }
    let mut so = ch.stdout.take().unwrap();
    let mut se = ch.stderr.take().unwrap();
    let ho = std::thread::spawn(move || { let mut b = vec![]; let _ = so.read_to_end(&mut b); b });
    let he = std::thread::spawn(move || { let mut b = vec![]; let _ = se.read_to_end(&mut b); b });
    let end = Instant::now() + Duration::from_secs(timeout);
    let code = loop {
        match ch.try_wait() {
            Ok(Some(s)) => break s.code().unwrap_or(1),
            Ok(None) if Instant::now() >= end => {
                let _ = ch.kill();
                let _ = ch.wait();
                return (124, String::new(), format!("{cmd} timed out"));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break 1,
        }
    };
    let o = String::from_utf8_lossy(&ho.join().unwrap_or_default()).into_owned();
    let e = String::from_utf8_lossy(&he.join().unwrap_or_default()).into_owned();
    (code, o, e)
}

fn nm_t(args: &[&str], input: Option<&str>, timeout: u64) -> (i32, String, String) {
    let (c, o, e) = exec(&nmcli(), args, input, timeout, true);
    match c {
        127 => (127, String::new(), "nmcli not found (install networkmanager)".into()),
        124 => (124, String::new(), "nmcli timed out".into()),
        _ => (c, o.trim().to_string(), e.trim().to_string()),
    }
}

fn nm(args: &[&str]) -> (i32, String, String) {
    nm_t(args, None, 60)
}

/// Split nmcli -t output on unescaped ':'.
pub fn split_t(line: &str) -> Vec<String> {
    let mut parts = vec![];
    let mut cur = String::new();
    let mut prev = '\0';
    for c in line.chars() {
        if c == ':' && prev != '\\' {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
        prev = c;
    }
    parts.push(cur);
    parts.into_iter().map(|p| p.replace("\\:", ":").replace("\\\\", "\\")).collect()
}

pub struct WSettings {
    pub self_correct: bool,
    pub allow_unvalidated: bool,
}
pub fn settings() -> WSettings {
    // root daemon: system-wide policy in /etc/shadow-tui/netd.toml; user tools: the user's config.toml
    let sys = std::fs::read_to_string(std::env::var("SHADOW_NETD_CONF").unwrap_or_else(|_| "/etc/shadow-tui/netd.toml".into())).unwrap_or_default();
    let get = |k: &str, d: bool| match st::get_in(&sys, "wifi", k).or_else(|| st::get("wifi", k)).as_deref() { Some("true") => true, Some("false") => false, _ => d };
    WSettings { self_correct: get("self_correct", true), allow_unvalidated: get("allow_unvalidated", false) }
}

pub fn available() -> bool {
    sys::which(&nmcli()).is_some()
}

fn nm_version() -> (u32, u32) {
    let out = nm(&["--version"]).1;
    let b = out.as_bytes();
    for i in 0..b.len() {
        if b[i].is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_digit()) {
            let rest = &out[i..];
            let maj: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Some(r) = rest[maj.len()..].strip_prefix('.') {
                let min: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
                if !min.is_empty() {
                    return (maj.parse().unwrap_or(0), min.parse().unwrap_or(0));
                }
            }
        }
    }
    (0, 0)
}

// ------------------------------------------------------------------ state
#[derive(Clone)]
pub struct Dev {
    pub device: String,
    pub state: String,
    pub connection: String,
}

pub fn wifi_device() -> Option<Dev> {
    let out = nm(&["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"]).1;
    out.lines().map(split_t).find(|f| f.len() >= 4 && f[1] == "wifi").map(|f| Dev { device: f[0].clone(), state: f[2].clone(), connection: f[3].clone() })
}

#[derive(Clone)]
#[allow(dead_code)]
pub struct Net {
    pub ssid: String,
    pub signal: i64,
    pub security: String,
    pub in_use: bool,
    pub enterprise: bool,
}

pub fn scan(rescan: bool) -> Vec<Net> {
    let out = nm_t(&["-t", "-f", "IN-USE,SIGNAL,SECURITY,SSID", "device", "wifi", "list", "--rescan", if rescan { "yes" } else { "no" }], None, 40).1;
    let mut nets: Vec<Net> = vec![];
    for line in out.lines() {
        let f = split_t(line);
        if f.len() < 4 || f[3].is_empty() {
            continue;
        }
        let (inuse, sig, sec, ssid) = (f[0] == "*", f[1].parse::<i64>().unwrap_or(0), f[2].clone(), f[3].clone());
        let pos = nets.iter().position(|n| n.ssid == ssid);
        let cur_inuse = pos.map_or(false, |i| nets[i].in_use);
        if pos.map_or(true, |i| sig > nets[i].signal) {
            let n = Net { ssid: ssid.clone(), signal: sig, enterprise: sec.contains("802.1X"), security: if sec.is_empty() { "open".into() } else { sec }, in_use: inuse || cur_inuse };
            match pos {
                Some(i) => nets[i] = n,
                None => nets.push(n),
            }
        }
    }
    nets.sort_by_key(|n| (!n.in_use, -n.signal));
    nets
}

pub fn connectivity() -> String {
    let out = nm_t(&["networking", "connectivity", "check"], None, 20).1;
    if out.is_empty() { "unknown".into() } else { out }
}

pub fn profiles() -> Vec<String> {
    nm(&["-t", "-f", "NAME,TYPE", "connection", "show"]).1.lines().filter(|l| l.ends_with(":802-11-wireless")).map(|l| split_t(l).remove(0)).collect()
}

pub fn prop(name: &str, key: &str) -> String {
    let (c, o, _) = nm(&["-s", "-g", key, "connection", "show", "id", name]);
    if c == 0 { o } else { String::new() }
}

// ------------------------------------------------------------------ profile creation
/// Store a secret without putting it on the command line (nmcli editor via stdin).
fn set_secret(name: &str, key: &str, value: &str) -> bool {
    let script = format!("set {key} {value}\nsave persistent\nyes\nquit\nyes\n");
    nm_t(&["connection", "edit", "id", name], Some(&script), 30);
    if prop(name, key) == value {
        return true;
    }
    nm(&["connection", "modify", "id", name, key, value]).0 == 0
}

fn create_enterprise(name: &str, ssid: &str, identity: &str, password: &str, domain: &str, priority: u32, user: &str) -> Result<(), String> {
    nm(&["connection", "delete", "id", name]);
    let prio = priority.to_string();
    let perm = format!("user:{user}");
    let mut args = vec!["connection", "add", "type", "wifi", "con-name", name, "ifname", "*", "ssid", ssid,
        "wifi-sec.key-mgmt", "wpa-eap", "802-1x.eap", "peap", "802-1x.phase2-auth", "mschapv2",
        "802-1x.identity", identity, "802-1x.password-flags", "0",
        "connection.autoconnect", "yes", "connection.autoconnect-priority", &prio];
    let ca = ca_bundle();
    if Path::new(&ca).exists() {
        args.extend(["802-1x.ca-cert", &ca]);
    }
    if !domain.is_empty() {
        args.extend(["802-1x.domain-suffix-match", domain]);
    }
    if !user.is_empty() {
        args.extend(["connection.permissions", &perm]);
    }
    let (c, o, e) = nm(&args);
    if c != 0 {
        return Err(if !e.is_empty() { e } else if !o.is_empty() { o } else { "could not create profile".into() });
    }
    set_secret(name, "802-1x.password", password);
    for fid in learned_for(name) {
        if let Some(f) = FIXES.iter().find(|f| f.id == fid) {
            if !f.root {
                apply_fix(f.id, &Ctx { name: name.into(), cb: None, ssid: ssid.into() });
            }
        }
    }
    Ok(())
}

pub fn setup_campus(preset: &str, user: &str, password: &str, realm: Option<&str>, cb: Cb) -> Result<Vec<String>, String> {
    let p = PRESETS.iter().find(|p| p.id == preset).ok_or("unknown preset")?;
    let mut realm = realm.unwrap_or(p.realm).trim().to_string();
    let mut user = user.trim().to_string();
    if user.contains('@') && p.identity == "{user}" {
        user = user.split('@').next().unwrap().to_string();
    } else if let Some((u, r)) = user.clone().split_once('@') {
        user = u.into();
        realm = r.into();
    }
    let identity = p.identity.replace("{user}", &user).replace("{realm}", &realm);
    let domain = p.domain.replace("{realm}", &realm);
    let owner = std::env::var("USER").unwrap_or_default();
    let mut visible: HashMap<String, String> = HashMap::new();
    for n in scan(false) {
        visible.insert(n.ssid.to_lowercase(), n.ssid);
    }
    let mut made = vec![];
    for (ssid, prio) in p.ssids {
        let real = visible.get(&ssid.to_lowercase()).cloned().unwrap_or_else(|| ssid.to_string());
        let name = format!("{PREFIX}{ssid}");
        create_enterprise(&name, &real, &identity, password, &domain, *prio, &owner)?;
        log(&format!("profile {name}: ssid={real} identity={identity} domain={domain}"), cb);
        made.push(name);
    }
    Ok(made)
}

pub fn connect_psk(ssid: &str, password: &str, cb: Cb) -> bool {
    let name = format!("{PREFIX}{ssid}");
    nm(&["connection", "delete", "id", &name]);
    let owner = std::env::var("USER").unwrap_or_default();
    let perm = format!("user:{owner}");
    let mut args = vec!["connection", "add", "type", "wifi", "con-name", &name, "ifname", "*", "ssid", ssid, "connection.autoconnect", "yes"];
    if !password.is_empty() {
        args.extend(["wifi-sec.key-mgmt", "wpa-psk", "wifi-sec.psk-flags", "0"]);
    }
    if !owner.is_empty() {
        args.extend(["connection.permissions", &perm]);
    }
    let (c, o, e) = nm(&args);
    if c != 0 {
        log(&format!("could not create {name}: {}", if e.is_empty() { o } else { e }), cb);
        return false;
    }
    if !password.is_empty() {
        set_secret(&name, "wifi-sec.psk", password);
    }
    connect_with_repair(&name, cb, true, true, 8, 0)
}

pub fn forget(name: &str) -> bool {
    nm(&["connection", "delete", "id", name]).0 == 0
}

// ------------------------------------------------------------------ diagnosis
const CATEGORIES: [(&str, &[&str]); 8] = [
    ("device", &[r"device .*not ready", r"not ready", r"device (is )?down", r"[Nn]o suitable device",
        r"device .* (is )?(not available|unavailable)", r"radio (is )?(switched off|disabled|off)",
        r"[Ww]i-?Fi (is )?disabled", r"unmanaged", r"NetworkManager is not running",
        r"[Nn]o Wi-?Fi device", r"rfkill", r"Could not find device", r"device .* not found",
        r"Device '[^']+' not found"]),
    ("tls", &[r"unsafe legacy renegotiation", r"SSL3 alert", r"handshake failure", r"routines::internal error",
        r"unsupported protocol", r"no protocols available", r"tlsv1 alert", r"wrong version number"]),
    ("cert", &[r"certificate verif\w* failed", r"CTRL-EVENT-EAP-TLS-CERT-ERROR", r"[Dd]omain suffix mismatch",
        r"unable to get local issuer", r"self.signed certificate", r"Server certificate (mismatch|chain)"]),
    ("auth", &[r"MSCHAPV2.*(fail|Authentication failed)", r"EAP-MSCHAPV2: Authentication failed",
        r"Secrets were required", r"auth_error", r"E=691", r"Access-Reject"]),
    ("notfound", &[r"could not be found", r"No network with SSID", r"SSID .* not found", r"network .*not available"]),
    ("assoc", &[r"4-Way Handshake failed", r"CTRL-EVENT-ASSOC-REJECT", r"association took too long",
        r"WRONG_KEY", r"reason=(2|15|23)\b", r"pmf", r"Association timed out"]),
    ("dhcp", &[r"IP configuration could not be reserved", r"ip-config-unavailable", r"DHCP.*time(d)? ?out", r"dhcp4 .* timeout"]),
    ("eap", &[r"802\.1X supplicant failed", r"CTRL-EVENT-EAP-FAILURE", r"supplicant-timeout", r"supplicant disconnected"]),
];
const ORDER: [&str; 8] = ["device", "tls", "cert", "notfound", "assoc", "dhcp", "eap", "auth"];

pub fn classify(text: &str) -> Vec<&'static str> {
    let t = rx::prep(text);
    let mut found: Vec<&'static str> = CATEGORIES.iter().filter(|(_, pats)| pats.iter().any(|p| rx::Rx::new(p).search(&t))).map(|(c, _)| *c).collect();
    if found.contains(&"device") {
        return vec!["device"];
    }
    if found.contains(&"eap") && found.iter().any(|c| matches!(*c, "tls" | "cert" | "auth")) {
        found.retain(|c| *c != "eap");
    }
    found
}

fn journal_since(t0: u64) -> String {
    let since = format!("@{}", t0 as i64 - 2);
    let (c, o, _) = exec(&journalctl(), &["--since", &since, "-o", "cat", "--no-pager", "-u", "NetworkManager", "-u", "wpa_supplicant", "-u", "iwd"], None, 15, false);
    if c == 127 || c == 124 {
        return String::new();
    }
    let ch: Vec<char> = o.chars().collect();
    ch[ch.len().saturating_sub(20000)..].iter().collect()
}

// ------------------------------------------------------------------ fixes
pub struct Ctx<'a> {
    name: String,
    cb: Cb<'a>,
    ssid: String,
}
impl<'a> Ctx<'a> {
    fn new(name: &str, cb: Cb<'a>) -> Ctx<'a> {
        Ctx { name: name.into(), cb, ssid: prop(name, "802-11-wireless.ssid") }
    }
}

pub struct Fix {
    pub id: &'static str,
    pub cat: &'static str,
    pub desc: &'static str,
    pub root: bool,
    pub risk: Option<&'static str>,
}
const fn fx(id: &'static str, cat: &'static str, desc: &'static str) -> Fix {
    Fix { id, cat, desc, root: false, risk: None }
}
pub const FIXES: [Fix; 20] = [
    fx("tls-legacy-versions", "tls", "allow TLS 1.0/1.1 for this network's login"),
    fx("tls-seclevel1", "tls", "OpenSSL compatibility for legacy TLS 1.2 RADIUS, scoped to this network (DEFAULT:@SECLEVEL=1)"),
    fx("tls-seclevel0", "tls", "lower OpenSSL security level further for this network only (DEFAULT@SECLEVEL=0)"),
    fx("peap-v1", "tls", "toggle the PEAP version (PEAPv0 ↔ PEAPv1) and reload the CA chain"),
    Fix { root: true, ..fx("wpa-legacy-renegotiation", "tls", "system fix: allow legacy TLS renegotiation in wpa_supplicant") },
    Fix { root: true, ..fx("iwd-backend", "tls", "system fix: switch NetworkManager to the iwd Wi-Fi backend") },
    fx("ca-system", "cert", "use the system certificate store for server validation"),
    Fix { risk: Some("weaker server check"), ..fx("relax-domain", "cert", "accept the server's certificate name variant (drop strict domain match)") },
    Fix { risk: Some("server not validated"), ..fx("no-ca-validation", "cert", "connect without validating the server certificate (you allowed this)") },
    fx("identity-format", "auth", "fix username format for this network"),
    fx("ssid-case", "notfound", "match the network name exactly as it is broadcast"),
    fx("pmf-off", "assoc", "disable protected management frames (driver/AP incompatibility)"),
    fx("mac-permanent", "assoc", "use the hardware MAC address (some networks reject randomised MACs)"),
    fx("powersave-off", "assoc", "disable Wi-Fi power saving (drops and stalls)"),
    fx("eap-tls-legacy", "eap", "allow TLS 1.0/1.1 for this network's login"),
    fx("eap-pmf-off", "eap", "disable protected management frames"),
    fx("ipv6-off", "dhcp", "disable IPv6 on this network (broken IPv6/DHCPv6)"),
    fx("dhcp-timeout", "dhcp", "wait longer for an address"),
    fx("dhcp-mac", "dhcp", "disable MAC randomisation for this campus network"),
    Fix { root: true, ..fx("dhcp-renew", "dhcp", "flush the neighbour/ARP table and renegotiate the DHCP lease (root)") },
];

fn modify(ctx: &Ctx, kv: &[&str]) -> bool {
    let mut a = vec!["connection", "modify", "id", &ctx.name];
    a.extend_from_slice(kv);
    let (c, o, e) = nm(&a);
    if c != 0 {
        log(&format!("   (could not apply: {})", if e.is_empty() { o } else { e }), ctx.cb);
    }
    c == 0
}

fn installed(cands: &[&str]) -> bool {
    cands.iter().any(|c| sys::which(c).is_some() || Path::new(c).exists())
}

fn run_helper(action: &str, ctx: &Ctx) -> bool {
    let h = helper();
    if !Path::new(&h).exists() {
        log("   system helper not installed", ctx.cb);
        return false;
    }
    let (c, o, e) = if sys::is_root() { exec(&h, &[action], None, 600, false) } else { exec("pkexec", &[&h, action], None, 600, false) };
    if c != 0 {
        let m = if !e.trim().is_empty() { e.trim().to_string() } else if !o.trim().is_empty() { o.trim().to_string() } else { "authorisation declined".into() };
        log(&format!("   system fix not applied ({m})"), ctx.cb);
        return false;
    }
    std::thread::sleep(Duration::from_secs(4));
    true
}

fn identity_variant(ctx: &Ctx) -> Option<String> {
    let ident = prop(&ctx.name, "802-1x.identity");
    let ssid = ctx.ssid.to_lowercase();
    if ssid == "eduroam" && !ident.contains('@') {
        let r = prop(&ctx.name, "802-1x.domain-suffix-match");
        return Some(format!("{ident}@{}", if r.is_empty() { "ucalgary.ca".into() } else { r }));
    }
    if ssid != "eduroam" && ident.contains('@') {
        return Some(ident.split('@').next().unwrap().to_string());
    }
    None
}

/// Visible network whose name differs only by case.
fn ssid_variant(ctx: &Ctx) -> Option<String> {
    let want = ctx.ssid.to_lowercase();
    scan(true).into_iter().map(|n| n.ssid).find(|v| v.to_lowercase() == want && *v != ctx.ssid)
}

fn fix_when(id: &str, c: &Ctx) -> bool {
    match id {
        "tls-seclevel0" | "tls-seclevel1" => nm_version() >= (1, 48),
        "dhcp-renew" => sys::is_root(),
        "iwd-backend" => sys::which("iwd").is_some() || Path::new("/usr/lib/iwd/iwd").exists(),
        "ca-system" => Path::new(CA_BUNDLE).exists() && prop(&c.name, "802-1x.ca-cert") != format!("file://{CA_BUNDLE}"),
        "relax-domain" => !prop(&c.name, "802-1x.domain-suffix-match").is_empty(),
        "no-ca-validation" => settings().allow_unvalidated,
        "identity-format" => identity_variant(c).is_some(),
        "ssid-case" => ssid_variant(c).is_some(),
        _ => true,
    }
}

fn apply_fix(id: &str, c: &Ctx) -> bool {
    match id {
        "tls-legacy-versions" | "eap-tls-legacy" => modify(c, &["802-1x.phase1-auth-flags", "tls-1-0-enable,tls-1-1-enable"]),
        "tls-seclevel1" => modify(c, &["802-1x.openssl-ciphers", "DEFAULT:@SECLEVEL=1"]),
        "tls-seclevel0" => modify(c, &["802-1x.openssl-ciphers", "DEFAULT@SECLEVEL=0"]),
        "peap-v1" => {
            let cur = prop(&c.name, "802-1x.phase1-peapver");
            let next = if cur == "1" { "0" } else { "1" };
            modify(c, &["802-1x.phase1-peapver", next, "802-1x.ca-cert", &ca_bundle()])
        }
        "dhcp-mac" => modify(c, &["802-11-wireless.cloned-mac-address", "permanent", "802-11-wireless.mac-address-randomization", "never"]),
        "dhcp-renew" => {
            let dev = wifi_device().map(|d| d.device).unwrap_or_default();
            if dev.is_empty() || !sys::is_root() {
                return false;
            }
            sys::run(&["ip", "neigh", "flush", "dev", &dev]);
            nm(&["device", "reapply", &dev]).0 == 0
        }
        "wpa-legacy-renegotiation" => run_helper("legacy-tls-on", c),
        "iwd-backend" => run_helper("iwd-on", c),
        "ca-system" => modify(c, &["802-1x.ca-cert", CA_BUNDLE, "802-1x.system-ca-certs", "yes"]),
        "relax-domain" => modify(c, &["802-1x.domain-suffix-match", ""]),
        "no-ca-validation" => modify(c, &["802-1x.ca-cert", "", "802-1x.system-ca-certs", "no"]),
        "identity-format" => {
            let v = identity_variant(c).unwrap_or_default();
            modify(c, &["802-1x.identity", &v])
        }
        "ssid-case" => {
            let v = ssid_variant(c).unwrap_or_default();
            modify(c, &["802-11-wireless.ssid", &v])
        }
        "pmf-off" | "eap-pmf-off" => modify(c, &["802-11-wireless-security.pmf", "1"]),
        "mac-permanent" => modify(c, &["802-11-wireless.cloned-mac-address", "permanent"]),
        "powersave-off" => modify(c, &["802-11-wireless.powersave", "2"]),
        "ipv6-off" => modify(c, &["ipv6.method", "disabled"]),
        "dhcp-timeout" => modify(c, &["ipv4.dhcp-timeout", "90"]),
        _ => false,
    }
}

fn learned() -> Vec<(String, Value)> {
    std::fs::read_to_string(learned_file()).ok().and_then(|t| json::parse(&t).ok()).map(|v| v.entries().to_vec()).unwrap_or_default()
}

fn learned_for(name: &str) -> Vec<String> {
    learned().into_iter().find(|(k, _)| k == name).and_then(|(_, v)| if let Value::Arr(a) = v { Some(a.iter().filter_map(|x| x.str().map(String::from)).collect()) } else { None }).unwrap_or_default()
}

fn remember(name: &str, fixes: &[String]) {
    let mut data = learned();
    let mut set: Vec<String> = learned_for(name);
    set.extend(fixes.iter().cloned());
    set.sort();
    set.dedup();
    let v = Value::Arr(set.into_iter().map(Value::Str).collect());
    match data.iter_mut().find(|(k, _)| k == name) {
        Some(e) => e.1 = v,
        None => data.push((name.into(), v)),
    }
    let _ = std::fs::create_dir_all(state());
    let _ = std::fs::write(learned_file(), crate::pyfmt::dumps(&Value::Obj(data), true));
}

fn up(name: &str) -> (bool, String) {
    let (c, o, e) = nm_t(&["-w", "45", "connection", "up", "id", name], None, 60);
    (c == 0, if e.is_empty() { o } else { e })
}

/// UCalgary: if this SSID isn't in range, the other one may be.
fn other_campus_profile(name: &str) -> Option<String> {
    let pair = [format!("{PREFIX}UC-Secure"), format!("{PREFIX}AirUC-Secure")];
    let i = pair.iter().position(|p| p == name)?;
    let other = pair[1 - i].clone();
    profiles().contains(&other).then_some(other)
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Connect `name`; on failure diagnose, apply the next fix and retry.
pub fn connect_with_repair(name: &str, cb: Cb, allow_root: bool, allow_auth: bool, max_steps: usize, hops: u32) -> bool {
    let mut applied: Vec<String> = vec![];
    let mut device_tries = 0;
    if !ensure_device(cb, allow_root, allow_root, true) {
        log("cannot connect until the Wi-Fi device works (see above)", cb);
        return false;
    }
    log(&format!("connecting {name}…"), cb);
    for step in 0..=max_steps {
        let t0 = now_s();
        let (ok, err) = up(name);
        if ok {
            log(&format!("connected: {name}{}", if applied.is_empty() { String::new() } else { format!(" (fixes: {})", applied.join(", ")) }), cb);
            let eap = if prop(name, "802-1x.eap").is_empty() { "PSK OK".to_string() } else { format!("PEAP/{} OK", if applied.iter().any(|a| a.starts_with("tls")) { "TLS1.2" } else { "TLS" }) };
            publish(Some(&eap));
            if !applied.is_empty() {
                remember(name, &applied);
            }
            let conn = connectivity();
            if conn == "limited" || conn == "portal" {
                log(&format!("connected but internet is '{conn}'{}", if conn == "portal" { " — a sign-in page may be required: opening browser" } else { "" }), cb);
                if conn == "portal" && sys::which("xdg-open").is_some() {
                    st::spawn_detached(&["xdg-open", "http://neverssl.com"]);
                }
            }
            return true;
        }
        let text = format!("{err}\n{}", journal_since(t0));
        let mut cats = classify(&text);
        if cats.is_empty() {
            cats = vec!["eap"];
        }
        let last = err.lines().last().unwrap_or("");
        log(&format!("attempt {} failed: {} → diagnosis: {}", step + 1, if err.is_empty() { "no detail" } else { last }, cats.join(", ")), cb);
        if cats.contains(&"device") {
            device_tries += 1;
            if device_tries > 2 || !ensure_device(cb, allow_root, allow_root, true) {
                log("the Wi-Fi device keeps failing — not retrying the network", cb);
                return false;
            }
            continue;
        }
        if cats.contains(&"notfound") && hops == 0 {
            if let Some(other) = other_campus_profile(name) {
                if !fix_when("ssid-case", &Ctx::new(name, cb)) {
                    log(&format!("{name} is not in range here — trying {other} (UCalgary is mid-rename)"), cb);
                    return connect_with_repair(&other, cb, allow_root, allow_auth, max_steps, 1);
                }
            }
        }
        let ctx = Ctx::new(name, cb);
        cats.sort_by_key(|c| ORDER.iter().position(|o| o == c).unwrap_or(99));
        let mut fix: Option<&Fix> = None;
        'outer: for cat in &cats {
            for f in FIXES.iter() {
                if f.cat != *cat || applied.iter().any(|a| a == f.id) || (f.root && !allow_root) || (f.cat == "auth" && !allow_auth) {
                    continue;
                }
                if fix_when(f.id, &ctx) {
                    fix = Some(f);
                    break 'outer;
                }
            }
        }
        publish(Some(&format!("{} FAIL", cats.first().copied().unwrap_or("eap").to_uppercase())));
        let Some(fix) = fix else {
            if cats.contains(&"auth") {
                log("password/username rejected — re-enter your credentials (not retrying, to avoid an account lock-out)", cb);
            } else if cats.contains(&"cert") && !settings().allow_unvalidated {
                log("server certificate could not be verified; enable 'allow unvalidated server' only if your institution says to", cb);
            } else {
                log("no further automatic fixes for this failure", cb);
            }
            return false;
        };
        log(&format!("applying fix: {}{}", fix.desc, fix.risk.map(|r| format!(" [{r}]")).unwrap_or_default()), cb);
        applied.push(fix.id.into());
        apply_fix(fix.id, &ctx);
    }
    log("giving up after the maximum number of repair steps", cb);
    false
}

// ------------------------------------------------------------------ device self-correction
// "device not ready" = NetworkManager state `unavailable`; "device down" = link down.
fn rd(p: &Path) -> String {
    std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn sorted_dir(p: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(p).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    v.sort();
    v
}

pub fn wifi_ifaces() -> Vec<String> {
    sorted_dir(&sysfs().join("class/net")).into_iter().filter(|d| d.join("wireless").exists() || d.join("phy80211").exists()).map(|d| d.file_name().unwrap().to_string_lossy().into_owned()).collect()
}

#[derive(Default, Clone)]
pub struct Rfkill {
    pub soft: bool,
    pub hard: bool,
    pub names: Vec<String>,
}

pub fn rfkill_state() -> Rfkill {
    let mut r = Rfkill::default();
    for d in sorted_dir(&sysfs().join("class/rfkill")) {
        if !d.file_name().unwrap().to_string_lossy().starts_with("rfkill") || rd(&d.join("type")) != "wlan" {
            continue;
        }
        r.names.push(rd(&d.join("name")));
        r.soft |= rd(&d.join("soft")) == "1";
        r.hard |= rd(&d.join("hard")) == "1";
    }
    r
}

fn link_up(iface: &str) -> bool {
    let base = sysfs().join("class/net").join(iface);
    let f = rd(&base.join("flags"));
    let h = f.trim_start_matches("0x").trim_start_matches("0X");
    match i64::from_str_radix(h, 16) {
        Ok(v) if !h.is_empty() => v & 1 != 0,
        _ => !matches!(rd(&base.join("operstate")).as_str(), "down" | ""),
    }
}

fn nm_running() -> bool {
    let (c, o, _) = nm_t(&["-t", "-f", "RUNNING", "general"], None, 10);
    c == 0 && o.contains("running")
}

fn radio_on() -> bool {
    nm_t(&["radio", "wifi"], None, 10).1.trim() == "enabled"
}

pub fn svc_active(unit: &str) -> bool {
    exec(&systemctl(), &["is-active", "-q", unit], None, 5, false).0 == 0
}

fn nm_backend_iwd() -> bool {
    let mut files = vec![etc().join("NetworkManager/NetworkManager.conf")];
    files.extend(sorted_dir(&etc().join("NetworkManager/conf.d")).into_iter().filter(|f| f.extension().map_or(false, |e| e == "conf")));
    files.iter().any(|f| {
        rd(f).lines().any(|l| {
            let Some(r) = l.trim_start().strip_prefix("wifi.backend") else { return false };
            let Some(r) = r.trim_start().strip_prefix('=') else { return false };
            r.trim_start().strip_prefix("iwd").map_or(false, |rest| !rest.chars().next().map_or(false, |c| c.is_alphanumeric() || c == '_'))
        })
    })
}

fn supplicant_problems() -> Vec<String> {
    let v = |s: &str| vec![s.to_string()];
    if nm_backend_iwd() {
        if !installed(&["iwd", "/usr/lib/iwd/iwd"]) {
            return v("iwd-missing");
        }
        return if svc_active("iwd.service") { vec![] } else { v("supplicant-down") };
    }
    if !installed(&["wpa_supplicant", "/usr/bin/wpa_supplicant"]) {
        return v("no-supplicant");
    }
    if svc_active("iwd.service") {
        return v("iwd-conflict");
    }
    if svc_active("wpa_supplicant.service") { vec![] } else { v("supplicant-down") }
}

#[derive(Default, Clone)]
pub struct Info {
    pub ifaces: Vec<String>,
    pub rfkill: Rfkill,
    pub nm: bool,
    pub radio: Option<bool>,
    pub device: Option<String>,
    pub state: Option<String>,
    pub link: Option<bool>,
    pub supplicant: Option<String>,
}

fn has(p: &[String], x: &str) -> bool {
    p.iter().any(|y| y == x)
}

/// (info, problems): ordered problem codes; empty = device usable.
pub fn device_health() -> (Info, Vec<String>) {
    let mut info = Info { ifaces: wifi_ifaces(), rfkill: rfkill_state(), nm: nm_running(), ..Default::default() };
    let mut probs: Vec<String> = vec![];
    let add = |p: &mut Vec<String>, s: &str| p.push(s.to_string());
    if !info.nm {
        add(&mut probs, "nm-down");
        if info.ifaces.is_empty() {
            add(&mut probs, "no-iface");
        }
        return (info, probs);
    }
    let d = wifi_device();
    info.radio = Some(radio_on());
    if let Some(d) = &d {
        info.device = Some(d.device.clone());
        info.state = Some(d.state.clone());
    }
    if info.rfkill.hard {
        add(&mut probs, "hard-block");
    }
    if info.rfkill.soft {
        add(&mut probs, "soft-block");
    }
    if info.radio == Some(false) {
        add(&mut probs, "radio-off");
    }
    if d.is_none() && info.ifaces.is_empty() {
        add(&mut probs, "no-iface");
        return (info, probs);
    }
    let iface = info.device.clone().unwrap_or_else(|| info.ifaces[0].clone());
    info.device = Some(iface.clone());
    let st = info.state.clone().unwrap_or_default();
    if d.is_none() || st.starts_with("unmanaged") {
        add(&mut probs, "unmanaged");
    } else if st.starts_with("unavailable") {
        add(&mut probs, "not-ready");
    }
    if (has(&probs, "not-ready") || has(&probs, "unmanaged")) && !["radio-off", "soft-block", "hard-block"].iter().any(|x| has(&probs, x)) {
        probs.extend(supplicant_problems());
    }
    info.supplicant = Some(if nm_backend_iwd() { "iwd" } else { "wpa_supplicant" }.into());
    if info.ifaces.contains(&iface) {
        let l = link_up(&iface);
        info.link = Some(l);
        if !l {
            add(&mut probs, "link-down");
        }
    }
    (info, probs)
}

pub fn problem_text(p: &str) -> &str {
    match p {
        "nm-down" => "NetworkManager is not running",
        "no-iface" => "no Wi-Fi interface (driver or firmware not loaded)",
        "hard-block" => "Wi-Fi is switched OFF in hardware (key/switch/BIOS)",
        "soft-block" => "Wi-Fi is soft-blocked (rfkill / airplane mode)",
        "radio-off" => "Wi-Fi radio is turned off in NetworkManager",
        "unmanaged" => "NetworkManager is not managing the Wi-Fi device",
        "not-ready" => "device not ready (NetworkManager: unavailable)",
        "link-down" => "device down (interface link is down)",
        "supplicant-down" => "the Wi-Fi supplicant (wpa_supplicant/iwd) is not running",
        "iwd-conflict" => "iwd and wpa_supplicant are both trying to drive the card",
        "no-supplicant" => "no Wi-Fi supplicant installed (wpa_supplicant)",
        "iwd-missing" => "NetworkManager is set to use iwd, but iwd is not installed",
        o => o,
    }
}
/// radio switched off / airplane mode: may be the user's choice — only fixed when asked to
const RADIO_CHOICE: [&str; 2] = ["radio-off", "soft-block"];
fn radio_choice(p: &[String]) -> bool {
    RADIO_CHOICE.iter().any(|x| has(p, x))
}

pub fn describe(probs: &[String]) -> String {
    if probs.is_empty() {
        return "Wi-Fi device OK".into();
    }
    probs.iter().map(|p| problem_text(p)).collect::<Vec<_>>().join("; ")
}

pub fn device_log_tail(n: usize) -> Vec<String> {
    let t = std::fs::read(device_log()).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let l: Vec<String> = t.lines().map(String::from).collect();
    l[l.len().saturating_sub(n)..].to_vec()
}

/// The 'Fix device' action: everything, including switching the radio back on.
pub fn fix_device(cb: Cb) -> bool {
    let ok = ensure_device(cb, true, true, true);
    if ok {
        let (info, _) = device_health();
        if let (Some(d), true) = (&info.device, info.state.as_deref().unwrap_or("").starts_with("disconnected")) {
            nm_t(&["device", "connect", d], None, 40);
        }
    }
    ok
}

/// Run a fixed device action as root. Never prompts from the background guard.
fn run_dev_helper(action: &str, cb: Cb, interactive: bool) -> bool {
    let h = dev_helper();
    if !Path::new(&h).exists() {
        log("   device helper not installed", cb);
        return false;
    }
    let (c, o, e) = if sys::is_root() {
        exec(&h, &[action], None, 120, false)
    } else if interactive {
        exec("pkexec", &[&h, action], None, 120, false)
    } else {
        exec("pkexec", &["--disable-internal-agent", &h, action], None, 120, false)
    };
    if c == 124 {
        log(&format!("   device helper failed: timed out after 120 seconds"), cb);
        return false;
    }
    if c == 127 {
        log(&format!("   device helper failed: {}", e.trim()), cb);
        return false;
    }
    for line in o.lines() {
        if !line.trim().is_empty() {
            log(&format!("   {}", line.trim()), cb);
        }
    }
    if c != 0 {
        log(&format!("   device fix not applied ({})", if e.trim().is_empty() { "authorisation declined" } else { e.trim() }), cb);
        return false;
    }
    true
}

fn wait_ready(seconds: f64) -> (Info, Vec<String>) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    loop {
        let (info, probs) = device_health();
        if probs.iter().all(|p| p == "hard-block") || Instant::now() >= end {
            return (info, probs);
        }
        std::thread::sleep(Duration::from_millis(1500));
    }
}

struct Step {
    id: &'static str,
    targets: &'static [&'static str],
    desc: &'static str,
}
const DEVICE_STEPS: [Step; 9] = [
    Step { id: "nm-start", targets: &["nm-down"], desc: "start and enable NetworkManager" },
    Step { id: "backend", targets: &["supplicant-down", "iwd-conflict", "no-supplicant", "iwd-missing"], desc: "make one Wi-Fi supplicant serve NetworkManager (wpa_supplicant / iwd) (root)" },
    Step { id: "radio-on", targets: &["soft-block", "radio-off"], desc: "turn the Wi-Fi radio on (clear rfkill soft-block)" },
    Step { id: "unblock", targets: &["soft-block", "radio-off", "link-down"], desc: "clear rfkill block and bring the interface up (root)" },
    Step { id: "managed", targets: &["unmanaged"], desc: "let NetworkManager manage the Wi-Fi device" },
    Step { id: "release", targets: &["unmanaged", "not-ready"], desc: "stop other network services holding the card (root)" },
    Step { id: "restart", targets: &["not-ready", "link-down", "unmanaged"], desc: "restart NetworkManager and the Wi-Fi supplicant (root)" },
    Step { id: "scan-mac", targets: &["not-ready"], desc: "disable scan MAC randomisation (driver compatibility, root)" },
    Step { id: "reload", targets: &["not-ready", "link-down", "no-iface"], desc: "reload the Wi-Fi driver (root)" },
];

/// None = a no-root step (no success signal), Some(ok) = helper result.
fn run_step(id: &str, cb: Cb, inter: bool) -> Option<bool> {
    match id {
        "nm-start" | "unblock" => Some(run_dev_helper("ensure", cb, inter)),
        "backend" => Some(run_dev_helper("backend", cb, inter)),
        "release" => Some(run_dev_helper("release", cb, inter)),
        "restart" => Some(run_dev_helper("restart", cb, inter)),
        "scan-mac" => Some(run_dev_helper("scan-mac-off", cb, inter)),
        "reload" => Some(run_dev_helper("reload-driver", cb, inter)),
        "radio-on" => {
            nm(&["radio", "wifi", "on"]);
            if sys::which("rfkill").is_some() {
                sys::run(&["rfkill", "unblock", "wifi"]);
            }
            None
        }
        "managed" => {
            let (info, _) = device_health();
            let list = if !info.ifaces.is_empty() { info.ifaces.clone() } else { info.device.iter().cloned().collect() };
            for i in list {
                nm(&["device", "set", &i, "managed", "yes"]);
            }
            None
        }
        _ => Some(false),
    }
}

/// Make the Wi-Fi device usable. force_radio=false (background guard): Wi-Fi switched off /
/// airplane mode is left alone.
pub fn ensure_device(cb: Cb, interactive: bool, allow_root: bool, force_radio: bool) -> bool {
    let (mut info, mut probs) = device_health();
    if probs.is_empty() {
        return true;
    }
    if !force_radio && probs.iter().all(|p| RADIO_CHOICE.contains(&p.as_str()) || p == "link-down" || p == "not-ready") && radio_choice(&probs) {
        return false;
    }
    log(&format!("Wi-Fi device problem: {}", describe(&probs)), cb);
    let known = learned_for(DEVICE_LEARNED);
    let mut steps: Vec<&Step> = DEVICE_STEPS.iter().collect();
    steps.sort_by_key(|s| !known.iter().any(|k| k == s.id));
    if has(&probs, "hard-block") {
        steps.clear();
    }
    let no_driver_sw = |ps: &[String]| (has(ps, "no-supplicant") && !installed(&["iwd", "/usr/lib/iwd/iwd"])) || has(ps, "iwd-missing");
    let mut used: Vec<String> = vec![];
    for s in steps {
        let live: Vec<&String> = probs.iter().filter(|p| *p != "hard-block").collect();
        if live.is_empty() || (no_driver_sw(&probs) && s.id != "nm-start" && used.iter().any(|u| u == "backend")) {
            break;
        }
        if !s.targets.iter().any(|t| live.iter().any(|l| l == t)) {
            continue;
        }
        if !force_radio && s.id == "radio-on" {
            continue;
        }
        let is_root = s.desc.contains("(root)") || s.id == "nm-start";
        if is_root && !allow_root {
            continue;
        }
        log(&format!("device fix: {}", s.desc), cb);
        used.push(s.id.into());
        let res = run_step(s.id, cb, interactive);
        let wait = if res == Some(false) { 0.0 } else if is_root { 12.0 } else { 6.0 };
        (info, probs) = wait_ready(wait);
        log(&format!("   → {}", describe(&probs)), cb);
    }
    if has(&probs, "hard-block") {
        log("Wi-Fi is switched off in hardware — press the Wi-Fi/airplane key (often Fn+F2…F12), flip the switch, or enable WLAN in the BIOS. Shadow TUI cannot override a hardware switch.", cb);
    }
    if has(&probs, "no-iface") {
        log("no Wi-Fi interface: the driver or firmware is missing. Install linux-firmware (and e.g. broadcom-wl for Broadcom cards), then reboot. `shadow wifi device --diag` shows kernel errors.", cb);
    }
    if has(&probs, "no-supplicant") {
        log(&format!("install the Wi-Fi supplicant once (needs a cable or phone USB tethering): {}", crate::distro::install_hint(&["wpa_supplicant"])), cb);
    }
    let ok = probs.is_empty();
    if ok && !used.is_empty() {
        remember(DEVICE_LEARNED, &used[used.len() - 1..]);
        log(&format!("Wi-Fi device ready ({}: {})", info.device.as_deref().unwrap_or("None"), info.state.as_deref().unwrap_or("None")), cb);
    } else if !ok {
        log(&format!("Wi-Fi device still not usable: {}", describe(&probs)), cb);
    }
    ok
}

// ------------------------------------------------------------------ guard (background self-correction)
/// Watch managed profiles; reconnect + repair (non-password fixes only).
pub fn guard(once: bool, cb: Cb, interval: u64) {
    guard_loop(once, cb, interval, false)
}

/// `root`: running as shadow-netd (system service) — device repairs and root fixes are allowed,
/// password changes never are.
fn guard_loop(once: bool, cb: Cb, interval: u64, root: bool) {
    let mut device_bad: u32 = 0;
    let mut hist: HashMap<String, Vec<u64>> = HashMap::new();
    loop {
        if settings().self_correct {
            let (_, mut probs) = device_health();
            if radio_choice(&probs) || has(&probs, "hard-block") {
                device_bad = 0;
                probs.clear();
            }
            if !probs.is_empty() {
                // root fixes belong to the system watcher (shadow-wifi-device.service)
                device_bad += 1;
                if device_bad == 2 || (root && device_bad % 6 == 0) {
                    log(&format!("guard: {} — self-correcting the device", describe(&probs)), cb);
                    ensure_device(cb, false, root, false);
                } else if device_bad == 6 {
                    let (_, probs) = device_health();
                    if !probs.is_empty() && !radio_choice(&probs) {
                        let how = if svc_active(WATCHER) { "the system repair service is working on it" } else { "open Settings → Wi-Fi → Fix device" };
                        log(&format!("guard: still {} — {how}", describe(&probs)), cb);
                        st::notify("Shadow TUI Wi-Fi", &format!("{} — {how}", describe(&probs)), "critical");
                    }
                }
            } else if device_bad >= 2 {
                log("guard: Wi-Fi device ready again", cb);
                device_bad = 0;
            } else {
                device_bad = 0;
            }
            let d = wifi_device();
            let mine: Vec<String> = profiles().into_iter().filter(|p| p.starts_with(PREFIX)).collect();
            let now = now_s();
            match &d {
                Some(d) if !mine.is_empty() && d.state == "disconnected" => {
                    let visible: Vec<String> = scan(true).into_iter().map(|n| n.ssid.to_lowercase()).collect();
                    for name in &mine {
                        let ssid = prop(name, "802-11-wireless.ssid").to_lowercase();
                        if !visible.contains(&ssid) {
                            continue;
                        }
                        let recent: Vec<u64> = hist.get(name).cloned().unwrap_or_default().into_iter().filter(|t| now - t < 3600).collect();
                        if recent.len() >= 3 {
                            continue;
                        }
                        let mut r = recent;
                        r.push(now);
                        hist.insert(name.clone(), r);
                        log(&format!("guard: {name} in range but not connected — self-correcting"), cb);
                        let ok = connect_with_repair(name, cb, root, false, 8, 0);
                        st::notify("Shadow TUI Wi-Fi", &format!("{}: {}", &name[PREFIX.len()..], if ok { "reconnected" } else { "could not reconnect — open Settings → Wi-Fi" }), if ok { "normal" } else { "critical" });
                        break;
                    }
                }
                Some(d) if d.state == "connected" && root => {
                    let c = connectivity();
                    if c == "limited" || c == "none" {
                        let key = format!("recover:{}", d.connection);
                        let recent: Vec<u64> = hist.get(&key).cloned().unwrap_or_default().into_iter().filter(|t| now - t < 3600).collect();
                        if recent.len() < 3 {
                            let mut r = recent;
                            r.push(now);
                            hist.insert(key, r);
                            net_recover(&d.device, cb);
                        }
                    }
                }
                Some(d) if d.state == "connected" && d.connection.starts_with(PREFIX) => {
                    if connectivity() == "limited" {
                        let key = format!("limited:{}", d.connection);
                        let recent: Vec<u64> = hist.get(&key).cloned().unwrap_or_default().into_iter().filter(|t| now - t < 3600).collect();
                        if recent.len() < 2 {
                            let mut r = recent;
                            r.push(now);
                            hist.insert(key, r);
                            log(&format!("guard: {} connected without internet — renewing", d.connection), cb);
                            nm(&["device", "reapply", &d.device]);
                        }
                    }
                }
                _ => {}
            }
        }
        if root {
            publish(None);
        }
        if once {
            return;
        }
        std::thread::sleep(Duration::from_secs(interval));
    }
}

// ------------------------------------------------------------------ shadow-netd (root daemon)
fn last_eap() -> &'static std::sync::Mutex<String> {
    static E: OnceLock<std::sync::Mutex<String>> = OnceLock::new();
    E.get_or_init(|| std::sync::Mutex::new("-".into()))
}

/// Write /run/shadow/net.state for the HUD: ssid, signal (dBm), EAP/TLS state.
pub fn publish(eap: Option<&str>) {
    if let Some(e) = eap {
        *last_eap().lock().unwrap() = e.to_string();
    }
    let d = wifi_device();
    let ssid = d.as_ref().filter(|d| d.state == "connected").map(|d| { let s = prop(&d.connection, "802-11-wireless.ssid"); if s.is_empty() { d.connection.clone() } else { s } }).unwrap_or_default();
    let dbm = std::fs::read_to_string("/proc/net/wireless").ok().and_then(|t| t.lines().nth(2).and_then(|l| l.split_whitespace().nth(3).map(|v| v.trim_end_matches('.').to_string()))).unwrap_or_default();
    let eap = if ssid.is_empty() { "-".to_string() } else { last_eap().lock().unwrap().clone() };
    st::write_run_state("net.state", &[("ssid", ssid), ("dbm", dbm), ("eap", eap), ("state", d.map(|d| d.state).unwrap_or_else(|| "none".into()))]);
}

/// Connected but no internet: gateway unreachable → flush neighbours + renew; gateway fine but
/// names don't resolve → switch this link to verified DNS-over-TLS resolvers.
fn net_recover(dev: &str, cb: Cb) {
    let gw = sys::run(&["ip", "-4", "route", "show", "default", "dev", dev]).1.split_whitespace().skip_while(|w| *w != "via").nth(1).map(String::from);
    let gw_ok = gw.as_deref().map(|g| sys::run(&["ping", "-c", "1", "-W", "2", g]).0 == 0).unwrap_or(false);
    if !gw_ok {
        log(&format!("netd: {dev} connected but the gateway does not answer — flushing ARP and renewing the lease"), cb);
        sys::run(&["ip", "neigh", "flush", "dev", dev]);
        nm(&["device", "reapply", dev]);
        return;
    }
    let dns_ok = sys::run(&["getent", "ahosts", "archlinux.org"]).0 == 0;
    if !dns_ok && sys::which("resolvectl").is_some() {
        log(&format!("netd: DNS from the campus DHCP server is failing — using DNS-over-TLS (Quad9 / Cloudflare) on {dev}"), cb);
        sys::run(&["resolvectl", "dns", dev, "9.9.9.9#dns.quad9.net", "1.1.1.1#cloudflare-dns.com"]);
        sys::run(&["resolvectl", "dnsovertls", dev, "yes"]);
        sys::run(&["resolvectl", "flush-caches"]);
    }
}

pub fn netd_main(args: &[&str]) -> i32 {
    if !sys::is_root() && !args.contains(&"--user") {
        eprintln!("shadow-netd runs as root (systemd: shadow-netd.service); use --user for a per-user guard");
        return 1;
    }
    let pr = |l: &str| eprintln!("{l}");
    let cb: Cb = Some(&pr);
    if !available() {
        eprintln!("shadow-netd: nmcli not found — install networkmanager");
        return 1;
    }
    let interval = std::env::var("SHADOW_NETD_INTERVAL").ok().and_then(|v| v.parse().ok()).unwrap_or(15);
    guard_loop(args.contains(&"--once"), cb, interval, sys::is_root());
    0
}

/// iwd backend (spec §4.2): write /var/lib/iwd/<SSID>.8021x provisioning files, root-only.
pub fn provision_iwd(preset: &str, user: &str, password: &str, realm: Option<&str>) -> Result<Vec<String>, String> {
    let p = PRESETS.iter().find(|p| p.id == preset).ok_or("unknown preset")?;
    let realm = realm.unwrap_or(p.realm).trim().to_string();
    let user = if p.identity == "{user}" { user.split('@').next().unwrap_or(user).to_string() } else { user.to_string() };
    let identity = p.identity.replace("{user}", user.split('@').next().unwrap_or(&user)).replace("{realm}", &realm);
    let domain = p.domain.replace("{realm}", &realm);
    let dir = PathBuf::from(std::env::var("SHADOW_IWD_DIR").unwrap_or_else(|_| "/var/lib/iwd".into()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut made = vec![];
    for (ssid, _) in p.ssids {
        let body = format!(
            "# written by shadow wifi iwd-provision ({preset})\n[Security]\nEAP-Method=PEAP\nEAP-Identity=anonymous@{realm}\nEAP-PEAP-CACert={}\nEAP-PEAP-ServerDomainMask=*.{domain}\nEAP-PEAP-Phase2-Method=MSCHAPV2\nEAP-PEAP-Phase2-Identity={identity}\nEAP-PEAP-Phase2-Password={}\n\n[Settings]\nAutoConnect=true\n",
            ca_bundle(), password.replace('\n', "")
        );
        let f = dir.join(format!("{ssid}.8021x"));
        std::fs::write(&f, body).map_err(|e| format!("{}: {e}", f.display()))?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600));
        made.push(f.display().to_string());
    }
    Ok(made)
}

// ------------------------------------------------------------------ CLI
fn read_line() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s.trim_end_matches('\n').trim_end_matches('\r').to_string()
}

fn getpass(prompt: &str) -> String {
    eprint!("{prompt}");
    let tty = std::io::stdin().is_terminal();
    if tty {
        let _ = Command::new("stty").arg("-echo").stdin(Stdio::inherit()).status();
    }
    let s = read_line();
    if tty {
        let _ = Command::new("stty").arg("echo").stdin(Stdio::inherit()).status();
        eprintln!();
    }
    s
}

fn die(m: &str) -> i32 {
    eprintln!("{m}");
    1
}

const USAGE: &str = "usage: shadow wifi status | scan [--tsv] | connect SSID [--password-stdin]\n                      | campus ucalgary|eduroam [--user U] [--realm R] [--password-stdin]\n                      | repair [PROFILE] | forget PROFILE | guard [--once] | log\n                      | device [--fix] [--diag] | netd [--once] | iwd-provision ucalgary|eduroam --user U [--password-stdin]";

pub fn main(args: &[&str]) -> i32 {
    let pr = |l: &str| {
        use std::io::Write;
        println!("{l}");
        let _ = std::io::stdout().flush();
    };
    let cb: Cb = Some(&pr);
    let sub = args.first().copied().unwrap_or("status");
    if !available() {
        return die("nmcli not found — install networkmanager and enable NetworkManager.service");
    }
    let exit = |b: bool| if b { 0 } else { 1 };
    let opt = |k: &str| args.iter().position(|a| *a == k).and_then(|i| args.get(i + 1)).copied();
    match sub {
        "status" => {
            let d = wifi_device();
            let (dev, st, conn) = match &d {
                Some(d) => (d.device.clone(), d.state.clone(), d.connection.clone()),
                None => (String::new(), "no Wi-Fi device".into(), String::new()),
            };
            println!("device      : {} ({st})", if dev.is_empty() { "-" } else { &dev });
            let (_, probs) = device_health();
            println!("health      : {}", describe(&probs));
            println!("connection  : {}", if conn.is_empty() { "-" } else { &conn });
            println!("internet    : {}", connectivity());
            println!("self-correct: {}", if settings().self_correct { "on" } else { "off" });
            0
        }
        "scan" => {
            for n in scan(true) {
                if args.contains(&"--tsv") {
                    println!("{}\t{}\t{}\t{}", if n.in_use { "*" } else { " " }, n.signal, n.security, n.ssid);
                } else {
                    let k = (n.signal.div_euclid(25) + 1).clamp(1, 4) as usize;
                    let bars: String = "▂▄▆█".chars().take(k).collect();
                    println!("{} {:<4} {:>3}%  {:<16} {}", if n.in_use { "●" } else { " " }, bars, n.signal, n.security, n.ssid);
                }
            }
            0
        }
        "connect" if args.len() >= 2 => {
            let ssid = args[1];
            let mine = format!("{PREFIX}{ssid}");
            let stdin_pw = args.contains(&"--password-stdin");
            if !stdin_pw && profiles().contains(&mine) {
                return exit(connect_with_repair(&mine, cb, true, true, 8, 0));
            }
            let pw = if stdin_pw { read_line() } else if std::io::stdin().is_terminal() { getpass(&format!("Password for {ssid} (empty for open network): ")) } else { String::new() };
            exit(connect_psk(ssid, &pw, cb))
        }
        "campus" if args.len() >= 2 && PRESETS.iter().any(|p| p.id == args[1]) => {
            let user = match opt("--user") {
                Some(u) => u.to_string(),
                None => {
                    print!("Username: ");
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                    read_line()
                }
            };
            let pw = if args.contains(&"--password-stdin") { read_line() } else { getpass("Password: ") };
            let made = match setup_campus(args[1], &user, &pw, opt("--realm"), cb) {
                Ok(m) => m,
                Err(e) => return die(&format!("shadow wifi: {e}")),
            };
            let visible: Vec<String> = scan(false).into_iter().map(|n| n.ssid.to_lowercase()).collect();
            let target = made.iter().find(|m| visible.contains(&prop(m, "802-11-wireless.ssid").to_lowercase())).unwrap_or(&made[0]).clone();
            exit(connect_with_repair(&target, cb, true, true, 8, 0))
        }
        "repair" => {
            let name = args.get(1).map(|s| s.to_string()).or_else(|| wifi_device().map(|d| d.connection).filter(|c| !c.is_empty())).or_else(|| profiles().into_iter().find(|p| p.starts_with(PREFIX)));
            let Some(mut name) = name else { return exit(ensure_device(cb, true, true, true)) };
            if !name.starts_with(PREFIX) && profiles().contains(&format!("{PREFIX}{name}")) {
                name = format!("{PREFIX}{name}");
            }
            exit(connect_with_repair(&name, cb, true, true, 8, 0))
        }
        "forget" if args.len() >= 2 => {
            let n = if args[1].starts_with(PREFIX) { args[1].to_string() } else { format!("{PREFIX}{}", args[1]) };
            exit(forget(&n))
        }
        "device" => {
            if args.contains(&"--diag") {
                run_dev_helper("diag", cb, true);
            }
            let (info, probs) = device_health();
            let py = |b: Option<bool>| match b { Some(true) => "True", Some(false) => "False", None => "None" };
            println!("interfaces : {}", if info.ifaces.is_empty() { "none".into() } else { info.ifaces.join(" ") });
            println!("NM         : {}   radio: {}   rfkill soft/hard: {}/{}", if info.nm { "running" } else { "NOT running" }, py(info.radio), py(Some(info.rfkill.soft)), py(Some(info.rfkill.hard)));
            println!("device     : {}   state: {}   link up: {}", info.device.as_deref().unwrap_or("-"), info.state.as_deref().filter(|s| !s.is_empty()).unwrap_or("-"), py(info.link));
            println!("supplicant : {}   system repair service: {}", info.supplicant.as_deref().unwrap_or("-"), if svc_active(WATCHER) { "running".to_string() } else { format!("not running (sudo systemctl enable --now {WATCHER})") });
            println!("health     : {}", describe(&probs));
            let tail = device_log_tail(6);
            if !tail.is_empty() {
                println!("recent system repairs:");
                for l in tail {
                    println!("  {l}");
                }
            }
            if args.contains(&"--fix") {
                return exit(fix_device(cb));
            }
            exit(probs.is_empty())
        }
        "fix-device" => exit(fix_device(cb)),
        "guard" => {
            guard(args.contains(&"--once"), cb, 20);
            0
        }
        "netd" => netd_main(&args[1..]),
        "iwd-provision" if args.len() >= 2 => {
            let user = match opt("--user") { Some(u) => u.to_string(), None => return die("iwd-provision: --user is required") };
            let pw = if args.contains(&"--password-stdin") { read_line() } else { getpass("Password: ") };
            match provision_iwd(args[1], &user, &pw, opt("--realm")) {
                Ok(f) => { for x in f { println!("wrote {x}"); } 0 }
                Err(e) => die(&format!("iwd-provision: {e}")),
            }
        }
        "log" => {
            match std::fs::read_to_string(wifi_log()) {
                Ok(t) => {
                    let l: Vec<&str> = t.lines().collect();
                    println!("{}", l[l.len().saturating_sub(60)..].join("\n"));
                }
                Err(_) => println!("(no Wi-Fi log yet)"),
            }
            0
        }
        _ => die(USAGE),
    }
}
