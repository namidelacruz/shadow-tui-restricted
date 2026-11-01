//! shadow-sec — the optional security subsystem (CR-SPEC-SHADOW-001 §8–9), shipped as its own
//! package (shadow-tui-sec). Nothing here runs or changes the system until you enable a module, and
//! a module can only be enabled after its precheck passes on this OS.
//!
//!   shadow-sec status                    what is enabled and what each control actually enforces
//!   shadow-sec precheck [MODULE…]        what this OS supports: can each module be enabled here?
//!   shadow-sec console                   the security console: precheck, enable / disable with your
//!                                        password (sudo is asked again every time) and a typed confirm
//!   shadow-sec enable|disable MODULE     usb zones sandbox audit fss ids verity crypto
//!   shadow-sec verify                    journal seals, fs-verity digests, crypto posture
//!   shadow-sec report                    ITSG-33 control mapping (self-assessment, not a certification)
//!   shadow-sec zeroize --plan|--execute  cryptographic erase of LUKS2 headers + learned weights
//!   shadow-sec daemon                    USB lockdown while the session is locked + intrusion monitor
#[path = "../../powerd/src/sys.rs"]
#[allow(dead_code)]
mod sys;
#[path = "../../core/src/distro.rs"]
#[allow(dead_code)]
mod distro;

use std::path::{Path, PathBuf};
use std::time::Duration;

const MODULES: [(&str, &str); 8] = [
    ("usb", "USB + Thunderbolt lockdown while the session is locked (kernel authorized_default)"),
    ("zones", "nftables zones: drop unsolicited inbound and scans at the ingress hook; management daemons denied the network"),
    ("sandbox", "systemd sandboxing for the Shadow daemons (read-only system, private /tmp, syscall filter, no new privileges)"),
    ("audit", "auditd rules for privilege use, healer patches, USB authorisation, Wi-Fi profiles, ptrace, memfd, module loads"),
    ("fss", "journald Forward Secure Sealing (tamper-evident logs, verified by `shadow-sec verify`)"),
    ("ids", "behaviour monitor: reverse shells, fileless (memfd) execution, foreign ptrace → SEC-ALERT in the HUD"),
    ("verity", "fs-verity on /opt/shadow-tui binaries (kernel-verified Merkle tree on every read)"),
    ("crypto", "SSH post-quantum hybrid key exchange + TLS 1.3 for git; LUKS2/swap/lockdown/Secure Boot posture"),
];

fn etc(rel: &str) -> PathBuf {
    sys::p(&format!("etc/{rel}"))
}
fn state() -> PathBuf {
    sys::p("var/lib/shadow-sec")
}
fn enabled(m: &str) -> bool {
    state().join(format!("{m}.on")).exists()
}
fn mark(m: &str, on: bool) {
    let _ = std::fs::create_dir_all(state());
    let f = state().join(format!("{m}.on"));
    if on { let _ = std::fs::write(f, ""); } else { let _ = std::fs::remove_file(f); }
}
fn need_root() -> Result<(), String> {
    if sys::is_root() || std::env::var_os("SHADOW_OPT_ROOT").is_some() { Ok(()) } else { Err("run as root (sudo shadow-sec …)".into()) }
}
fn write(path: &Path, body: &str) -> Result<(), String> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, body).map_err(|e| format!("{}: {e}", path.display()))
}
fn alert(why: &str) {
    let d = sys::p("run/shadow");
    let _ = std::fs::create_dir_all(&d);
    let _ = std::fs::write(d.join("sec.alert"), format!("{why}\n"));
    sys::log_line(&state().join("events.log"), why);
}

// ---------------------------------------------------------------- usb / thunderbolt
fn usb_buses() -> Vec<String> {
    sys::ls("sys/bus/usb/devices").into_iter().filter(|d| d.starts_with("usb")).collect()
}
/// locked: new USB devices (BadUSB / keystroke injectors) stay unauthorised until unlock
fn usb_lock(lock: bool) {
    for b in usb_buses() {
        let _ = sys::write(&format!("sys/bus/usb/devices/{b}/authorized_default"), if lock { "0" } else { "1" });
    }
    if !lock {
        // devices plugged in while locked: authorise now that the owner is back
        for d in sys::ls("sys/bus/usb/devices") {
            if sys::read(&format!("sys/bus/usb/devices/{d}/authorized")).as_deref() == Some("0") {
                let _ = sys::write(&format!("sys/bus/usb/devices/{d}/authorized"), "1");
                sys::log_line(&state().join("events.log"), &format!("usb: authorised {d} after unlock"));
            }
        }
    }
}
fn session_locked() -> bool {
    std::env::var_os("SHADOW_SEC_LOCKED").map(|v| v == "1").unwrap_or_else(|| {
        sys::ls("run/user").iter().any(|u| sys::exists(&format!("run/user/{u}/shadow/locked")))
    })
}
fn thunderbolt_level() -> String {
    sys::ls("sys/bus/thunderbolt/devices").iter().find_map(|d| sys::read(&format!("sys/bus/thunderbolt/devices/{d}/security"))).unwrap_or_else(|| "no controller".into())
}

// ---------------------------------------------------------------- zones (nftables)
const NFT: &str = r#"#!/usr/sbin/nft -f
# Shadow TUI Restricted host zones (shadow-sec zones). Public Access Zone = everything arriving on
# Wi-Fi/Ethernet; only replies to connections this host opened are accepted.
table inet shadow_zones
delete table inet shadow_zones
table inet shadow_zones {
    chain ingress_scan_guard {
        # early drop of classic scan signatures (NULL / XMAS / SYN+FIN)
        type filter hook prerouting priority raw; policy accept;
        tcp flags & (fin|syn|rst|psh|ack|urg) == 0x0 drop
        tcp flags & (fin|psh|urg) == fin|psh|urg drop
        tcp flags & (syn|fin) == syn|fin drop
    }
    chain input {
        type filter hook input priority filter; policy drop;
        iif "lo" accept
        ct state established,related accept
        ct state invalid drop
        udp sport 67 udp dport 68 accept                 # DHCP replies
        udp sport 547 udp dport 546 accept               # DHCPv6 replies
        icmpv6 type { nd-neighbor-solicit, nd-neighbor-advert, nd-router-advert, packet-too-big } accept
        icmp type echo-request limit rate 2/second accept
        counter drop
    }
    chain forward {
        type filter hook forward priority filter; policy drop;
    }
}
"#;
const MZ_DROPIN: &str = "# shadow-sec zones: Restricted Management Zone — no network for this daemon\n[Service]\nIPAddressDeny=any\nIPAddressAllow=localhost\nRestrictAddressFamilies=AF_UNIX AF_NETLINK\n";

// ---------------------------------------------------------------- sandbox
const SANDBOX: &str = "# shadow-sec sandbox\n[Service]\nNoNewPrivileges=yes\nPrivateTmp=yes\nProtectSystem=strict\nProtectHome=read-only\nProtectKernelModules=yes\nProtectKernelLogs=yes\nProtectControlGroups=yes\nProtectClock=yes\nRestrictRealtime=yes\nRestrictSUIDSGID=yes\nLockPersonality=yes\nSystemCallArchitectures=native\nSystemCallFilter=@system-service\nSystemCallErrorNumber=EPERM\n";
/// what each daemon still has to write
fn sandbox_paths(unit: &str) -> &'static str {
    match unit {
        "shadow-powerd.service" => "ReadWritePaths=/sys /run /var/lib/shadow-powerd /etc/shadow-tui\n",
        "shadow-netd.service" => "ReadWritePaths=/run /var/lib/iwd /etc/NetworkManager\nCapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_RAW CAP_SYS_ADMIN CAP_SYS_MODULE CAP_KILL\n",
        "shadow-healer.service" => "ReadWritePaths=/run /var/lib/shadow-tui\n",
        "shadow-audiod.service" => "ReadWritePaths=/run /sys\n",
        _ => "",
    }
}
const DAEMONS: [&str; 4] = ["shadow-powerd.service", "shadow-netd.service", "shadow-healer.service", "shadow-audiod.service"];

// ---------------------------------------------------------------- audit
const AUDIT: &str = "## shadow-sec audit — ITSG-33 AU-2 / AU-9 event selection\n\
-w /usr/bin/sudo -p x -k priv_esc\n-w /usr/bin/su -p x -k priv_esc\n-w /usr/bin/pkexec -p x -k priv_esc\n\
-w /var/lib/shadow-tui/modules -p wa -k healer_patch\n\
-w /etc/NetworkManager/system-connections -p wa -k wifi_profiles\n-w /var/lib/iwd -p wa -k wifi_profiles\n\
-a always,exit -F arch=b64 -S ptrace -k ptrace\n\
-a always,exit -F arch=b64 -S memfd_create -k memfd\n\
-a always,exit -F arch=b64 -S init_module,finit_module,delete_module -k kmod\n\
-w /etc/shadow-tui -p wa -k shadow_conf\n\
-w /etc/sudoers -p wa -k priv_conf\n-w /etc/sudoers.d -p wa -k priv_conf\n";

// ---------------------------------------------------------------- ids
/// processes whose stdin is a network socket and which are shells → reverse-shell signature
fn ids_scan() -> Vec<String> {
    let mut hits = vec![];
    for pid in sys::ls("proc").into_iter().filter(|p| p.chars().all(|c| c.is_ascii_digit())) {
        let comm = sys::read(&format!("proc/{pid}/comm")).unwrap_or_default();
        let exe = std::fs::read_link(sys::p(&format!("proc/{pid}/exe"))).map(|l| l.to_string_lossy().into_owned()).unwrap_or_default();
        if exe.starts_with("/memfd:") {
            hits.push(format!("pid {pid} ({comm}) executes from memory ({exe}) — fileless execution"));
        }
        if ["sh", "bash", "zsh", "dash", "fish", "nc", "ncat", "socat"].contains(&comm.as_str()) {
            let fd0 = std::fs::read_link(sys::p(&format!("proc/{pid}/fd/0"))).map(|l| l.to_string_lossy().into_owned()).unwrap_or_default();
            if fd0.starts_with("socket:") && is_inet_socket(&fd0) {
                hits.push(format!("pid {pid} ({comm}) has a network socket as its input — reverse shell pattern"));
            }
        }
        if let Some(tr) = sys::read(&format!("proc/{pid}/status")).and_then(|s| s.lines().find_map(|l| l.strip_prefix("TracerPid:").map(|v| v.trim().to_string()))) {
            if tr != "0" {
                let tc = sys::read(&format!("proc/{tr}/comm")).unwrap_or_default();
                if !["gdb", "lldb", "strace", "ltrace", "rr", "perf", "valgrind"].contains(&tc.as_str()) {
                    hits.push(format!("pid {pid} ({comm}) is being traced by {tr} ({tc}) — unexpected ptrace"));
                }
            }
        }
    }
    hits
}
fn is_inet_socket(link: &str) -> bool {
    let inode = link.trim_start_matches("socket:[").trim_end_matches(']');
    ["proc/net/tcp", "proc/net/tcp6"].iter().any(|f| sys::read(f).map_or(false, |t| t.lines().skip(1).any(|l| l.split_whitespace().nth(9) == Some(inode))))
}

// ---------------------------------------------------------------- verity
fn opt_binaries() -> Vec<PathBuf> {
    let d = sys::p("opt/shadow-tui/bin");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&d).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.is_file() && !p.is_symlink()).collect();
    v.sort();
    v
}

// ---------------------------------------------------------------- crypto posture
const SSH: &str = "# shadow-sec crypto — post-quantum hybrid key exchange first (OpenSSH 9.9+/10), strong host keys\nHost *\n    KexAlgorithms mlkem768x25519-sha256,sntrup761x25519-sha512,sntrup761x25519-sha512@openssh.com,curve25519-sha256\n    HostKeyAlgorithms ssh-ed25519,ecdsa-sha2-nistp384,sk-ssh-ed25519@openssh.com,rsa-sha2-512\n    Ciphers aes256-gcm@openssh.com,chacha20-poly1305@openssh.com\n    MACs hmac-sha2-512-etm@openssh.com\nHost github.com\n    PasswordAuthentication no\n    KbdInteractiveAuthentication no\n";

fn posture() -> Vec<(String, String)> {
    let mut v = vec![];
    let lockdown = sys::read("sys/kernel/security/lockdown").map(|l| l.split_whitespace().find(|w| w.starts_with('[')).unwrap_or("?").trim_matches(|c| c == '[' || c == ']').to_string()).unwrap_or_else(|| "unavailable".into());
    v.push(("kernel lockdown".into(), lockdown));
    let sb = sys::ls("sys/firmware/efi/efivars").iter().find(|f| f.starts_with("SecureBoot-")).and_then(|f| std::fs::read(sys::p(&format!("sys/firmware/efi/efivars/{f}"))).ok()).and_then(|b| b.last().copied());
    v.push(("Secure Boot".into(), match sb { Some(1) => "on".into(), Some(_) => "off".into(), None => "unknown (no EFI vars)".into() }));
    v.push(("TPM 2.0".into(), if sys::exists("sys/class/tpm/tpm0") { sys::read("sys/class/tpm/tpm0/tpm_version_major").map(|m| format!("present (v{m})")).unwrap_or("present".into()) } else { "not found".into() }));
    let luks: Vec<String> = sys::run(&["lsblk", "-rno", "NAME,FSTYPE"]).1.lines().filter(|l| l.ends_with("crypto_LUKS")).map(|l| l.split_whitespace().next().unwrap_or("").to_string()).collect();
    let luks_desc = if luks.is_empty() { "no LUKS volumes".to_string() } else {
        luks.iter().map(|d| {
            let dump = sys::run(&["cryptsetup", "luksDump", &format!("/dev/{d}")]).1;
            let ver = dump.lines().find_map(|l| l.strip_prefix("Version:")).map(|s| s.trim().to_string()).unwrap_or("?".into());
            let cipher = dump.lines().find_map(|l| l.trim().strip_prefix("cipher:")).map(|s| s.trim().to_string()).unwrap_or("?".into());
            format!("{d}: LUKS{ver} {cipher}")
        }).collect::<Vec<_>>().join(", ")
    };
    v.push(("disk encryption".into(), luks_desc));
    let swaps = sys::read("proc/swaps").unwrap_or_default();
    v.push(("swap".into(), if swaps.lines().count() <= 1 { "none".into() } else if swaps.contains("zram") && swaps.lines().count() == 2 { "zram only (RAM, gone at power-off)".into() } else { "disk swap present — make sure it is encrypted".into() }));
    let ssh = sys::run(&["ssh", "-Q", "kex"]).1;
    v.push(("SSH PQC hybrid KEX".into(), if ssh.contains("mlkem768x25519") { "available (ML-KEM-768 + X25519)".into() } else if ssh.contains("sntrup761") { "sntrup761 hybrid only (update OpenSSH for ML-KEM)".into() } else { "not available".into() }));
    let g = sys::run(&["git", "config", "--system", "--get", "http.sslVersion"]).1.trim().to_string();
    v.push(("git TLS".into(), if g.is_empty() { "system default (shadow-sec enable crypto → TLS 1.3)".into() } else { g }));
    v
}

// ---------------------------------------------------------------- precheck: what this OS supports
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Lvl {
    Ok,
    Warn,
    Fail,
}

/// One requirement of a module on this system: (level, what was found, how to fix it)
pub type Req = (Lvl, String, String);

fn kernel_has(module: &str, sysfs: &str) -> bool {
    sys::exists(sysfs) || sys::exists(&format!("sys/module/{module}")) || sys::run(&["modprobe", "-n", "-q", module]).0 == 0
}

fn systemd_version() -> u32 {
    sys::run(&["systemctl", "--version"]).1.split_whitespace().nth(1).and_then(|v| v.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()).unwrap_or(0)
}

fn unit_exists(u: &str) -> bool {
    ["usr/lib/systemd/system", "lib/systemd/system", "etc/systemd/system"].iter().any(|d| sys::exists(&format!("{d}/{u}")))
}

fn active(u: &str) -> bool {
    sys::run(&["systemctl", "is-active", "-q", u]).0 == 0
}

/// Filesystem type and source of the filesystem holding `path`
fn fs_of(path: &str) -> (String, String) {
    // the nearest existing directory (before the first install /opt/shadow-tui may not exist)
    let mut p = std::path::PathBuf::from(path);
    while !p.exists() && p.pop() {}
    let o = sys::run(&["findmnt", "-n", "-o", "FSTYPE,SOURCE", "-T", &p.to_string_lossy()]).1;
    let mut it = o.split_whitespace();
    (it.next().unwrap_or("?").to_string(), it.next().unwrap_or("").to_string())
}

pub fn precheck(m: &str) -> Vec<Req> {
    let ok = |w: &str| (Lvl::Ok, w.to_string(), String::new());
    let warn = |w: &str, f: &str| (Lvl::Warn, w.to_string(), f.to_string());
    let fail = |w: &str, f: &str| (Lvl::Fail, w.to_string(), f.to_string());
    let hint = |p: &[&str]| distro::install_hint(p);
    let mut v: Vec<Req> = vec![];
    let service = || {
        if unit_exists("shadow-sec.service") {
            if active("shadow-sec.service") { ok("shadow-sec.service running") } else { warn("shadow-sec.service is not running", "enabling this module starts it (systemctl enable --now shadow-sec)") }
        } else {
            fail("shadow-sec.service is missing", "reinstall the shadow-tui-sec package")
        }
    };
    match m {
        "usb" => {
            v.push(if sys::exists("sys/bus/usb/devices") && !usb_buses().is_empty() { ok(&format!("{} USB bus(es) with authorized_default", usb_buses().len())) } else { fail("no USB buses in /sys/bus/usb", "this kernel has no USB authorisation interface") });
            v.push(service());
            let tb = thunderbolt_level();
            v.push(match tb.as_str() {
                "no controller" => ok("no Thunderbolt controller"),
                "none" | "dponly" => warn(&format!("Thunderbolt security level '{tb}': PCIe tunnels are not gated"), "set 'user' or 'secure' in the firmware setup"),
                _ => ok(&format!("Thunderbolt security level '{tb}'")),
            });
        }
        "zones" => {
            v.push(if sys::which("nft").is_some() { ok("nft (nftables) installed") } else { fail("nft is not installed", &hint(&["nftables"])) });
            v.push(if kernel_has("nf_tables", "proc/net/netfilter") { ok("kernel nf_tables available") } else { fail("the kernel has no nf_tables", "a kernel built with CONFIG_NF_TABLES is required") });
            if active("firewalld.service") {
                v.push(warn("firewalld is active: its rules and these zones both apply (the stricter wins)", "fine as is; or stop firewalld if you want these zones alone"));
            }
            if active("ufw.service") {
                v.push(warn("ufw is active: its rules and these zones both apply", "fine as is"));
            }
            v.push(service());
        }
        "sandbox" => {
            let sv = systemd_version();
            v.push(if sv >= 247 { ok(&format!("systemd {sv} (sandbox options supported)")) } else { fail(&format!("systemd {sv} is too old (need 247+)"), "update the system") });
            let missing: Vec<&str> = DAEMONS.iter().copied().filter(|u| !unit_exists(u)).collect();
            v.push(if missing.is_empty() { ok("all Shadow daemon units present") } else { warn(&format!("units not found: {}", missing.join(", ")), "only the present ones are sandboxed") });
        }
        "audit" => {
            v.push(if sys::which("augenrules").is_some() || sys::which("auditctl").is_some() { ok("audit userspace installed") } else { fail("audit (auditd/augenrules) is not installed", &hint(&["audit"])) });
            let cmdline = sys::read("proc/cmdline").unwrap_or_default();
            v.push(if cmdline.split_whitespace().any(|a| a == "audit=0") { fail("the kernel was booted with audit=0", "remove audit=0 from the kernel command line and reboot") } else if sys::exists("proc/self/loginuid") { ok("kernel audit enabled") } else { fail("the kernel has no audit support", "a kernel built with CONFIG_AUDIT is required") });
            v.push(if active("auditd.service") { ok("auditd running") } else { warn("auditd is not running", "sudo systemctl enable --now auditd") });
        }
        "fss" => {
            let help = sys::run(&["journalctl", "--help"]).1;
            v.push(if help.contains("--setup-keys") { ok("journald supports Forward Secure Sealing") } else { fail("this systemd was built without FSS (no --setup-keys)", "not available on this distribution's systemd") });
            v.push(if sys::exists("var/log/journal") { ok("persistent journal") } else { warn("the journal is volatile (no /var/log/journal)", "enabling FSS makes it persistent") });
            v.push(warn("the verification key must be written down and kept OFF this machine", "it is shown once when you enable"));
        }
        "ids" => {
            v.push(ok("/proc is readable (process heuristics)"));
            v.push(service());
            v.push(if enabled("audit") { ok("audit rules enabled (better evidence)") } else { warn("audit is not enabled: detection uses /proc heuristics only", "enable 'audit' too") });
        }
        "verity" => {
            v.push(if sys::which("fsverity").is_some() { ok("fsverity-utils installed") } else { fail("fsverity-utils is not installed", &hint(&["fsverity-utils"])) });
            let (fs, src) = fs_of(&sys::p("opt/shadow-tui").to_string_lossy());
            match fs.as_str() {
                "ext4" => {
                    let kernel = sys::exists("sys/fs/ext4/features/verity");
                    let feat = sys::run(&["tune2fs", "-l", &src]).1.lines().any(|l| l.starts_with("Filesystem features") && l.contains("verity"));
                    v.push(if !kernel {
                        fail("the kernel's ext4 has no fs-verity", "a kernel with CONFIG_FS_VERITY is required")
                    } else if feat {
                        ok(&format!("ext4 on {src} has the verity feature"))
                    } else if !sys::is_root() {
                        // tune2fs needs root to read the superblock: enable re-checks as root
                        warn(&format!("ext4 on {src}: the verity feature can only be read as root"), "checked again when you enable (with your password)")
                    } else {
                        fail(&format!("ext4 on {src} lacks the verity feature"), &format!("tune2fs -O verity {src} (from a rescue system, unmounted)"))
                    });
                }
                "btrfs" | "f2fs" => v.push(if sys::exists(&format!("sys/fs/{fs}/features/verity")) { ok(&format!("{fs} with fs-verity support")) } else { fail(&format!("the kernel's {fs} has no fs-verity"), "a kernel with CONFIG_FS_VERITY is required") }),
                "xfs" => v.push(fail("/opt/shadow-tui is on XFS, which has no fs-verity (RHEL/Fedora server default)", "not supported on this filesystem; use 'sandbox' + 'audit' for integrity instead")),
                other => v.push(fail(&format!("/opt/shadow-tui is on {other}, which has no fs-verity"), "ext4 (with the verity feature), btrfs or f2fs is required")),
            }
        }
        "crypto" => {
            let kex = sys::run(&["ssh", "-Q", "kex"]).1;
            v.push(if sys::which("ssh").is_none() { fail("the OpenSSH client is not installed", &hint(&["openssh"])) } else if kex.contains("mlkem768x25519") { ok("OpenSSH with ML-KEM-768 hybrid key exchange") } else if kex.contains("sntrup761") { warn("OpenSSH offers sntrup761 hybrid only", "update OpenSSH (9.9+) for ML-KEM") } else { fail("OpenSSH has no post-quantum hybrid key exchange", "update OpenSSH to 9.0+") });
            v.push(if sys::which("git").is_some() { ok("git installed (TLS 1.3 setting)") } else { warn("git is not installed", "the git part is skipped") });
        }
        _ => v.push(fail(&format!("unknown module '{m}'"), "usb zones sandbox audit fss ids verity crypto")),
    }
    v
}

pub fn supported(m: &str) -> bool {
    !precheck(m).iter().any(|r| r.0 == Lvl::Fail)
}

/// The security features of the OS itself (for the console header and `precheck`)
fn platform() -> Vec<(String, String)> {
    let mut v = vec![];
    let os = sys::read("etc/os-release").unwrap_or_default();
    let name = os.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).unwrap_or("?").trim_matches('"').to_string();
    v.push(("system".into(), format!("{name} · kernel {}", sys::read("proc/sys/kernel/osrelease").unwrap_or_default())));
    let lsm = sys::read("sys/kernel/security/lsm").unwrap_or_default();
    let selinux = sys::read("sys/fs/selinux/enforce").map(|e| if e == "1" { "SELinux enforcing" } else { "SELinux permissive" });
    let aa = sys::exists("sys/module/apparmor/parameters/enabled").then_some("AppArmor");
    let named = [selinux, aa].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    v.push(("LSM".into(), match (named.is_empty(), lsm.is_empty()) {
        (true, true) => "none detected".into(),
        (true, false) => lsm,
        (false, true) => named,
        (false, false) => format!("{named}  ({lsm})"),
    }));
    v.extend(posture().into_iter().filter(|(k, _)| ["kernel lockdown", "Secure Boot", "TPM 2.0", "disk encryption"].contains(&k.as_str())));
    v
}

fn print_precheck(mods: &[&str], porcelain: bool) -> bool {
    let mut all_ok = true;
    if !porcelain {
        for (k, val) in platform() {
            println!("  {k:<16} {val}");
        }
        println!();
    }
    for m in mods {
        let r = precheck(m);
        let sup = !r.iter().any(|x| x.0 == Lvl::Fail);
        all_ok &= sup;
        if porcelain {
            for (l, w, f) in &r {
                println!("{m} {} {w}{}", match l { Lvl::Ok => "ok", Lvl::Warn => "warn", Lvl::Fail => "fail" }, if f.is_empty() { String::new() } else { format!(" | {f}") });
            }
            println!("{m} {} {}", if sup { "supported" } else { "unsupported" }, if enabled(m) { "enabled" } else { "off" });
            continue;
        }
        println!("  \x1b[1m{m:<8}\x1b[0m {}  {}", if sup { "\x1b[32mcan be enabled\x1b[0m" } else { "\x1b[31mnot supported here\x1b[0m" }, if enabled(m) { "(enabled)" } else { "" });
        for (l, w, f) in r {
            let mark = match l { Lvl::Ok => "\x1b[32m✓\x1b[0m", Lvl::Warn => "\x1b[33m!\x1b[0m", Lvl::Fail => "\x1b[31m✗\x1b[0m" };
            println!("      {mark} {w}{}", if f.is_empty() { String::new() } else { format!("\n          → {f}") });
        }
    }
    all_ok
}

// ---------------------------------------------------------------- the security console
const R: &str = "\x1b[0m";
const AMB: &str = "\x1b[38;2;255;176;0m";
const CYN: &str = "\x1b[38;2;0;229;255m";
const VIO: &str = "\x1b[38;2;155;107;255m";
const DIM: &str = "\x1b[38;2;138;132;116m";
const RED: &str = "\x1b[38;2;255;42;0m";
const GRN: &str = "\x1b[38;2;124;217;90m";

fn line(prompt: &str) -> Option<String> {
    use std::io::Write;
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut l = String::new();
    match std::io::stdin().read_line(&mut l) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(l.trim().to_string()),
    }
}

fn beam(w: usize) -> String {
    (0..w).map(|i| {
        let f = i as f32 / w as f32;
        let mix = |a: f32, b: f32, t: f32| (a + (b - a) * t) as u8;
        let (r, g, b) = if f < 0.5 { (mix(255.0, 155.0, f * 2.0), mix(176.0, 107.0, f * 2.0), mix(0.0, 255.0, f * 2.0)) } else { (mix(155.0, 0.0, f * 2.0 - 1.0), mix(107.0, 229.0, f * 2.0 - 1.0), 255) };
        format!("\x1b[38;2;{r};{g};{b}m─")
    }).collect::<String>() + R
}

/// Run `shadow-sec ARGS` with administrator rights, always asking for the password again.
fn privileged(args: &[&str]) -> i32 {
    let me = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "/opt/shadow-tui/bin/shadow-sec".into());
    if sys::is_root() {
        return std::process::Command::new(&me).args(args).status().map(|s| s.code().unwrap_or(1)).unwrap_or(1);
    }
    // forget any cached sudo credential: every change is authorised with the password, now
    let _ = std::process::Command::new("sudo").arg("-k").status();
    let mut c = std::process::Command::new("sudo");
    c.args(["-p", &format!("{AMB}[shadow-sec]{R} password for %u: "), "--", &me]).args(args);
    c.status().map(|s| s.code().unwrap_or(1)).unwrap_or(1)
}

fn console() -> i32 {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("shadow-sec console needs a terminal (open it from the launcher or: shadow-term -e shadow-sec console)");
        return 2;
    }
    let mods: Vec<&str> = MODULES.iter().map(|m| m.0).collect();
    loop {
        print!("\x1b[2J\x1b[H");
        println!("{AMB}╭─ ◢ SHADOW-SEC // SECURITY CONSOLE ─╮{R}  {DIM}nothing changes without your password{R}");
        println!("{}", beam(78));
        for (k, val) in platform() {
            println!("  {DIM}{k:<16}{R} {val}");
        }
        println!("{}", beam(78));
        println!("  {DIM}#  MODULE    STATE      ON THIS SYSTEM{R}");
        for (i, (m, d)) in MODULES.iter().enumerate() {
            let r = precheck(m);
            let sup = !r.iter().any(|x| x.0 == Lvl::Fail);
            let warns = r.iter().filter(|x| x.0 == Lvl::Warn).count();
            let st = if enabled(m) { format!("{GRN}enabled{R}  ") } else { format!("{DIM}off{R}      ") };
            let s = if !sup { format!("{RED}✗ not supported{R}") } else if warns > 0 { format!("{AMB}! ready ({warns} note{}){R}", if warns > 1 { "s" } else { "" }) } else { format!("{GRN}✓ ready{R}") };
            println!("  {CYN}{}{R}  {m:<8}  {st} {s}", i + 1);
            println!("     {DIM}{}{R}", d.chars().take(72).collect::<String>());
        }
        println!("{}", beam(78));
        println!("  {VIO}n{R} details · {VIO}e n{R} enable · {VIO}d n{R} disable · {VIO}v{R} verify · {VIO}r{R} report · {VIO}q{R} quit");
        let Some(cmd) = line(&format!("\n  {AMB}sec ›{R} ")) else { return 0 };
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let pick = |s: &str| s.parse::<usize>().ok().and_then(|n| mods.get(n.wrapping_sub(1)).copied()).or_else(|| mods.iter().copied().find(|m| *m == s));
        match parts.as_slice() {
            ["q"] | ["quit"] | ["exit"] => return 0,
            ["v"] => {
                privileged(&["verify"]);
                line(&format!("\n  {DIM}Enter to go back{R} "));
            }
            ["r"] => {
                report();
                line(&format!("\n  {DIM}Enter to go back{R} "));
            }
            [n] if pick(n).is_some() => {
                let m = pick(n).unwrap();
                println!();
                print_precheck(&[m], false);
                line(&format!("\n  {DIM}Enter to go back{R} "));
            }
            ["e", n] if pick(n).is_some() => {
                let m = pick(n).unwrap();
                println!();
                let sup = print_precheck(&[m], false);
                if enabled(m) {
                    println!("\n  {m} is already enabled.");
                } else if !sup {
                    println!("\n  {RED}{m} cannot be enabled on this system{R} — fix the ✗ items above first.");
                } else {
                    println!("\n  {AMB}This changes system configuration.{R} It is logged in /var/lib/shadow-sec/events.log.");
                    let c = line(&format!("  Type {CYN}{m}{R} to confirm: ")).unwrap_or_default();
                    if c == m {
                        let rc = privileged(&["enable", m]);
                        println!("\n  {}", if rc == 0 { format!("{GRN}✓ {m} enabled{R}") } else { format!("{RED}✗ {m} was not enabled (wrong password, or see above){R}") });
                    } else {
                        println!("  cancelled");
                    }
                }
                line(&format!("\n  {DIM}Enter to go back{R} "));
            }
            ["d", n] if pick(n).is_some() => {
                let m = pick(n).unwrap();
                let c = line(&format!("\n  Type {CYN}{m}{R} to confirm disabling it: ")).unwrap_or_default();
                if c == m {
                    let rc = privileged(&["disable", m]);
                    println!("\n  {}", if rc == 0 { format!("{GRN}✓ {m} disabled{R}") } else { format!("{RED}✗ not changed{R}") });
                } else {
                    println!("  cancelled");
                }
                line(&format!("\n  {DIM}Enter to go back{R} "));
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------- enable / disable
fn enable(m: &str) -> Result<String, String> {
    need_root()?;
    // the OS must support it: nothing half-configured
    let fails: Vec<String> = precheck(m).into_iter().filter(|r| r.0 == Lvl::Fail).map(|r| format!("{}{}", r.1, if r.2.is_empty() { String::new() } else { format!(" → {}", r.2) })).collect();
    if !fails.is_empty() {
        return Err(format!("{m} cannot be enabled on this system:\n  {}", fails.join("\n  ")));
    }
    let who = std::env::var("SUDO_USER").unwrap_or_else(|_| "root".into());
    sys::log_line(&state().join("events.log"), &format!("enable {m} by {who}"));
    if ["usb", "zones", "ids"].contains(&m) && unit_exists("shadow-sec.service") && std::env::var_os("SHADOW_OPT_ROOT").is_none() {
        sys::run(&["systemctl", "enable", "--now", "shadow-sec.service"]);
    }
    let r = match m {
        "usb" => {
            mark("usb", true);
            Ok(format!("USB lockdown armed: while a session is locked, new USB devices stay unauthorised (Thunderbolt security level: {})", thunderbolt_level()))
        }
        "zones" => {
            write(&etc("nftables.d/shadow-zones.nft"), NFT)?;
            let (c, _, e) = sys::run(&["nft", "-f", &etc("nftables.d/shadow-zones.nft").to_string_lossy()]);
            for u in ["shadow-powerd.service", "shadow-healer.service"] {
                write(&etc(&format!("systemd/system/{u}.d/50-shadow-zone.conf")), MZ_DROPIN)?;
            }
            sys::run(&["systemctl", "daemon-reload"]);
            if c != 0 { Err(format!("nft: {e}")) } else { Ok("zones active: inbound drop by default, scan signatures dropped at prerouting/raw; shadow-powerd and shadow-healer have no network (restart them to apply)".into()) }
        }
        "sandbox" => {
            for u in DAEMONS {
                write(&etc(&format!("systemd/system/{u}.d/40-shadow-sandbox.conf")), &format!("{SANDBOX}{}", sandbox_paths(u)))?;
            }
            sys::run(&["systemctl", "daemon-reload"]);
            Ok("sandbox drop-ins written for the four daemons; restart them (or reboot) to apply".into())
        }
        "audit" => {
            write(&etc("audit/rules.d/50-shadow-tui.rules"), AUDIT)?;
            let (c, _, e) = sys::run(&["augenrules", "--load"]);
            if c != 0 { Err(format!("rules written but not loaded ({e}); is the audit package installed and auditd running?")) } else { Ok("audit rules loaded (keys: priv_esc healer_patch wifi_profiles ptrace memfd kmod shadow_conf)".into()) }
        }
        "fss" => {
            write(&etc("systemd/journald.conf.d/50-shadow-seal.conf"), "[Journal]\nStorage=persistent\nSeal=yes\n")?;
            let (c, out, e) = sys::run(&["journalctl", "--setup-keys", "--interval=15min"]);
            sys::run(&["systemctl", "restart", "systemd-journald"]);
            if c != 0 { return Err(format!("journalctl --setup-keys: {e}")); }
            Ok(format!("journal sealing on. WRITE DOWN this verification key and keep it OFF this machine:\n{}\n(an attacker with root could otherwise reseal the logs)", out.lines().last().unwrap_or("")))
        }
        "ids" => {
            mark("ids", true);
            let freeze = std::env::var("SHADOW_SEC_FREEZE").is_ok();
            Ok(format!("behaviour monitor on (alerts in the HUD as SEC-ALERT){}", if freeze { "; offending processes will be stopped (SIGSTOP)" } else { "; set freeze = true in /etc/shadow-tui/sec.conf to also stop offending processes" }))
        }
        "verity" => {
            if sys::which("fsverity").is_none() {
                return Err(format!("fsverity-utils not installed ({})", distro::install_hint(&["fsverity-utils"])));
            }
            let mut digests = String::new();
            let mut n = 0;
            for f in opt_binaries() {
                let fs = f.to_string_lossy().into_owned();
                let (c, _, e) = sys::run(&["fsverity", "enable", "--hash-alg=sha512", &fs]);
                if c != 0 && !e.contains("File exists") {
                    return Err(format!("{fs}: {e} (the filesystem needs the verity feature: tune2fs -O verity, or btrfs)"));
                }
                digests.push_str(&sys::run(&["fsverity", "measure", &fs]).1);
                digests.push('\n');
                n += 1;
            }
            write(&etc("shadow-tui/verity.digests"), &digests)?;
            Ok(format!("fs-verity (SHA-512) on {n} binaries; digests recorded in /etc/shadow-tui/verity.digests. Note: package updates replace the files — run `shadow-sec enable verity` again after each update."))
        }
        "crypto" => {
            write(&etc("ssh/ssh_config.d/50-shadow-tui.conf"), SSH)?;
            sys::run(&["git", "config", "--system", "http.sslVersion", "tlsv1.3"]);
            Ok("SSH: ML-KEM/X25519 hybrid key exchange preferred, password auth to GitHub off; git: TLS 1.3 only".into())
        }
        _ => Err(format!("unknown module '{m}' (usb zones sandbox audit fss ids verity crypto)")),
    };
    if r.is_ok() {
        mark(m, true);
    }
    r
}

fn disable(m: &str) -> Result<String, String> {
    need_root()?;
    sys::log_line(&state().join("events.log"), &format!("disable {m} by {}", std::env::var("SUDO_USER").unwrap_or_else(|_| "root".into())));
    match m {
        "usb" => usb_lock(false),
        "zones" => {
            sys::run(&["nft", "delete", "table", "inet", "shadow_zones"]);
            let _ = std::fs::remove_file(etc("nftables.d/shadow-zones.nft"));
            for u in ["shadow-powerd.service", "shadow-healer.service"] {
                let _ = std::fs::remove_file(etc(&format!("systemd/system/{u}.d/50-shadow-zone.conf")));
            }
        }
        "sandbox" => {
            for u in DAEMONS {
                let _ = std::fs::remove_file(etc(&format!("systemd/system/{u}.d/40-shadow-sandbox.conf")));
            }
        }
        "audit" => {
            let _ = std::fs::remove_file(etc("audit/rules.d/50-shadow-tui.rules"));
            sys::run(&["augenrules", "--load"]);
        }
        "fss" => { let _ = std::fs::remove_file(etc("systemd/journald.conf.d/50-shadow-seal.conf")); }
        "crypto" => {
            let _ = std::fs::remove_file(etc("ssh/ssh_config.d/50-shadow-tui.conf"));
            sys::run(&["git", "config", "--system", "--unset", "http.sslVersion"]);
        }
        "ids" | "verity" => {}
        _ => return Err(format!("unknown module '{m}'")),
    }
    sys::run(&["systemctl", "daemon-reload"]);
    mark(m, false);
    Ok(format!("{m} disabled"))
}

// ---------------------------------------------------------------- verify / report / zeroize
fn verify() -> i32 {
    let mut bad = 0;
    if enabled("fss") {
        let (c, _, e) = sys::run(&["journalctl", "--verify", "-q"]);
        println!("journal seals : {}", if c == 0 { "intact".to_string() } else { bad += 1; format!("FAILED — {}", e.lines().next().unwrap_or("")) });
    }
    if enabled("verity") {
        let want = std::fs::read_to_string(etc("shadow-tui/verity.digests")).unwrap_or_default();
        let mut ok = 0;
        for l in want.lines().filter(|l| !l.is_empty()) {
            let file = l.split_whitespace().last().unwrap_or("");
            if sys::run(&["fsverity", "measure", file]).1.trim() == l.trim() { ok += 1 } else { bad += 1; println!("verity        : MISMATCH {file}"); }
        }
        println!("verity        : {ok} binaries match");
    }
    for (k, v) in posture() {
        println!("{k:<14}: {v}");
    }
    if bad > 0 {
        alert(&format!("verify: {bad} integrity failure(s)"));
    }
    if bad == 0 { 0 } else { 1 }
}

fn report() {
    let rows: [(&str, &str, &str); 14] = [
        ("AC-2/AC-7", "login + lock screen via PAM/greetd; idle lock", "core"),
        ("AC-11", "zero-information lock screen, idle timeout", "core"),
        ("IA-2(1)", "MFA: add pam_u2f / pam_pkcs11 to /etc/pam.d/greetd and shadow-lock (hardware token)", "manual"),
        ("AU-2/AU-12", "auditd rules (shadow-sec audit)", "audit"),
        ("AU-9", "journald Forward Secure Sealing (shadow-sec fss)", "fss"),
        ("CM-5/SI-7", "fs-verity on Shadow binaries (shadow-sec verity)", "verity"),
        ("CM-7", "keyboard-first compositor; no screen-capture protocol exposed", "core"),
        ("SC-7", "nftables host zones (shadow-sec zones)", "zones"),
        ("SC-8/SC-13", "TLS 1.3 for git; SSH ML-KEM hybrid KEX (shadow-sec crypto)", "crypto"),
        ("SC-28", "LUKS2 AES-256-XTS (set at install; reported by verify)", "posture"),
        ("SC-39", "systemd sandboxing of daemons (shadow-sec sandbox)", "sandbox"),
        ("SI-4", "behaviour monitor (shadow-sec ids)", "ids"),
        ("MP-6", "cryptographic erase (shadow-sec zeroize)", "manual"),
        ("SI-3", "USB lockdown while locked (shadow-sec usb)", "usb"),
    ];
    println!("Shadow TUI Restricted — control mapping (ITSG-33 families)");
    println!("SELF-ASSESSMENT ONLY. This is not a CSE, CMVP/FIPS 140-3 or CPCSC certification, and no");
    println!("software package can make a machine PROTECTED A/B on its own.\n");
    for (ctl, what, m) in rows {
        let st = match m {
            "core" => "in place",
            "manual" => "your action",
            "posture" => "see verify",
            m if enabled(m) => "enabled",
            _ => "available",
        };
        println!("{ctl:<11} {st:<12} {what}");
    }
    println!("\nNot implemented (named in the spec, outside what this package can honestly do):");
    for n in ["eBPF/XDP programs and an NPU-classified syscall model (ids uses /proc + audit heuristics instead)",
              "keystroke-dynamics biometrics", "TPM-signed model attestation and a golden recovery image",
              "per-client cryptographic portal tokens (clients simply get no capture/snoop protocols)",
              "FIPS 140-3 validated crypto modules inside the runtime", "UEFI key enrolment / UKI measurement (use sbctl + systemd-ukify)"] {
        println!("  · {n}");
    }
}

fn zeroize(args: &[&str]) -> i32 {
    let luks: Vec<String> = sys::run(&["lsblk", "-rpno", "NAME,FSTYPE"]).1.lines().filter(|l| l.ends_with("crypto_LUKS")).map(|l| l.split_whitespace().next().unwrap_or("").to_string()).collect();
    let weights = [sys::p("var/lib/shadow-powerd/learn.state"), sys::p("var/lib/shadow-tui/healer")];
    println!("ZEROIZE PLAN — cryptographic erase (ITSP.40.006)");
    for d in &luks {
        println!("  cryptsetup erase {d}    (destroys every keyslot: the data becomes unrecoverable)");
    }
    for w in &weights {
        println!("  shred + remove {}", w.display());
    }
    if luks.is_empty() {
        println!("  (no LUKS volumes found — nothing on disk can be cryptographically erased)");
    }
    if !args.contains(&"--execute") {
        println!("\nThis was the plan only. To run it: sudo shadow-sec zeroize --execute");
        return 0;
    }
    if let Err(e) = need_root() {
        eprintln!("{e}");
        return 1;
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("zeroize needs a person at a terminal");
        return 1;
    }
    let host = sys::read("etc/hostname").unwrap_or_else(|| "this-machine".into());
    let phrase = format!("ZEROIZE {}", host.to_uppercase());
    // hardware token check: a configured USB serial must be plugged in
    if let Some(serial) = sys::read("etc/shadow-tui/zeroize-token") {
        let present = sys::ls("sys/bus/usb/devices").iter().any(|d| sys::read(&format!("sys/bus/usb/devices/{d}/serial")).as_deref() == Some(serial.trim()));
        if !present {
            eprintln!("the hardware token configured in /etc/shadow-tui/zeroize-token is not plugged in");
            return 1;
        }
    }
    for round in 1..=2 {
        print!("Type '{phrase}' to confirm ({round}/2): ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut l = String::new();
        let _ = std::io::stdin().read_line(&mut l);
        if l.trim() != phrase {
            println!("not confirmed — nothing was erased");
            return 1;
        }
    }
    for d in &luks {
        let (c, _, e) = sys::run(&["cryptsetup", "erase", "-q", d]);
        println!("{d}: {}", if c == 0 { "keyslots erased".to_string() } else { format!("FAILED ({e})") });
    }
    for w in &weights {
        if w.is_file() {
            sys::run(&["shred", "-u", "-z", &w.to_string_lossy()]);
        } else {
            let _ = std::fs::remove_dir_all(w);
        }
    }
    println!("zeroize complete. Power off now: the volume keys are still in RAM until then.");
    0
}

fn daemon() -> i32 {
    // nftables rules don't survive a reboot on their own: restore the zones at start
    if enabled("zones") && sys::exists("etc/nftables.d/shadow-zones.nft") {
        let (c, _, e) = sys::run(&["nft", "-f", &etc("nftables.d/shadow-zones.nft").to_string_lossy()]);
        sys::log_line(&state().join("events.log"), &if c == 0 { "zones: restored at start".to_string() } else { format!("zones: restore failed: {e}") });
    }
    let mut was_locked = false;
    let freeze = std::fs::read_to_string(etc("shadow-tui/sec.conf")).unwrap_or_default().lines().any(|l| l.replace(' ', "") == "freeze=true");
    let mut seen: Vec<String> = vec![];
    loop {
        if enabled("usb") {
            let locked = session_locked();
            if locked != was_locked {
                usb_lock(locked);
                sys::log_line(&state().join("events.log"), if locked { "usb: locked — new devices blocked" } else { "usb: unlocked — devices allowed" });
                was_locked = locked;
            }
        }
        if enabled("ids") {
            for h in ids_scan() {
                if !seen.contains(&h) {
                    alert(&format!("ids: {h}"));
                    if freeze {
                        if let Some(pid) = h.split_whitespace().nth(1) {
                            sys::run(&["kill", "-STOP", pid]);
                            sys::log_line(&state().join("events.log"), &format!("ids: stopped pid {pid}"));
                        }
                    }
                    seen.push(h);
                }
            }
        }
        if std::env::var_os("SHADOW_SEC_ONCE").is_some() {
            return 0;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn status() {
    println!("shadow-sec — optional security subsystem (nothing is active until enabled)\n");
    for (m, d) in MODULES {
        println!("  [{}] {m:<8} {d}", if enabled(m) { "x" } else { " " });
    }
    println!("\n  alert   : {}", sys::read("run/shadow/sec.alert").unwrap_or_else(|| "none".into()));
    println!("  USB     : authorized_default {} on {} bus(es); Thunderbolt: {}", usb_buses().first().and_then(|b| sys::read(&format!("sys/bus/usb/devices/{b}/authorized_default"))).unwrap_or("-".into()), usb_buses().len(), thunderbolt_level());
    println!("\n  shadow-sec verify · shadow-sec report · shadow-sec zeroize --plan · clear an alert: sudo shadow-sec ack");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let pr = |r: Result<String, String>| match r {
        Ok(s) => { println!("{s}"); 0 }
        Err(e) => { eprintln!("shadow-sec: {e}"); 1 }
    };
    let code = match a.as_slice() {
        [] | ["status"] => { status(); 0 }
        ["enable", m] => pr(enable(m)),
        ["precheck", rest @ ..] => {
            let porcelain = rest.contains(&"--porcelain");
            let picked: Vec<&str> = rest.iter().copied().filter(|a| !a.starts_with("--")).collect();
            let mods: Vec<&str> = if picked.is_empty() { MODULES.iter().map(|m| m.0).collect() } else { picked };
            if print_precheck(&mods, porcelain) { 0 } else { 3 }
        }
        ["console"] => console(),
        ["disable", m] => pr(disable(m)),
        ["verify"] => verify(),
        ["report"] => { report(); 0 }
        ["zeroize", rest @ ..] => zeroize(rest),
        ["daemon"] => daemon(),
        ["scan"] => { let h = ids_scan(); for x in &h { println!("{x}"); } if h.is_empty() { println!("nothing suspicious"); } 0 }
        ["ack"] => pr(need_root().map(|_| { let _ = std::fs::remove_file(sys::p("run/shadow/sec.alert")); "alert cleared".to_string() })),
        _ => { eprintln!("usage: shadow-sec status | precheck [MODULE…] [--porcelain] | console | enable|disable MODULE | verify | report | scan | ack | zeroize --plan|--execute | daemon"); 2 }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod t {
    /// tests that point SHADOW_OPT_ROOT somewhere run one at a time
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn unsupported_modules_cannot_be_enabled() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let r = std::env::temp_dir().join(format!("secpre-{}", std::process::id()));
        std::fs::create_dir_all(r.join("proc")).unwrap();
        std::fs::write(r.join("proc/cmdline"), "quiet audit=0\n").unwrap();
        std::env::set_var("SHADOW_OPT_ROOT", &r);
        // no USB buses in this root, and the kernel was booted with audit=0
        assert!(!super::supported("usb"));
        let e = super::enable("audit").unwrap_err();
        assert!(e.contains("cannot be enabled") && e.contains("audit=0"), "{e}");
        assert!(!r.join("etc/audit/rules.d/50-shadow-tui.rules").exists(), "nothing was written");
        assert!(super::precheck("nonsense").iter().any(|x| x.0 == super::Lvl::Fail));
        std::env::remove_var("SHADOW_OPT_ROOT");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn usb_lockdown_follows_the_lock_screen() {
        let _g = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let r = std::env::temp_dir().join(format!("sec-{}", std::process::id()));
        for d in ["usb1", "usb2", "1-1"] {
            std::fs::create_dir_all(r.join(format!("sys/bus/usb/devices/{d}"))).unwrap();
        }
        std::fs::write(r.join("sys/bus/usb/devices/usb1/authorized_default"), "1").unwrap();
        std::fs::write(r.join("sys/bus/usb/devices/usb2/authorized_default"), "1").unwrap();
        std::fs::write(r.join("sys/bus/usb/devices/1-1/authorized"), "0").unwrap();
        std::env::set_var("SHADOW_OPT_ROOT", &r);
        super::usb_lock(true);
        assert_eq!(std::fs::read_to_string(r.join("sys/bus/usb/devices/usb1/authorized_default")).unwrap(), "0");
        super::usb_lock(false);
        assert_eq!(std::fs::read_to_string(r.join("sys/bus/usb/devices/usb2/authorized_default")).unwrap(), "1");
        assert_eq!(std::fs::read_to_string(r.join("sys/bus/usb/devices/1-1/authorized")).unwrap(), "1"); // plugged in while locked → authorised after unlock
        std::env::remove_var("SHADOW_OPT_ROOT");
    }
}
