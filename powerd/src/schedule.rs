//! When the habit model learns, and how fast.
//!
//!   first week      7 days of learning: the model watches and fits quickly, but makes no
//!                   power decisions yet (firmware defaults + the AC/battery policy apply)
//!   learned         predictions drive PL1/EPP/profile; learning continues slowly
//!                   (dynamic self-learning), and apps it hasn't seen still learn fast
//!   OS update       a full system upgrade (pacman -Syu / dnf upgrade) or a new kernel starts a fresh 7-day
//!                   relearn: the same apps can behave differently on a new kernel/Mesa/firmware
//!   drift           if predictions keep missing (error EWMA > 0.25 over 40+ samples) while
//!                   learned, a shorter relearn (2 days) starts
//!
//! State: /var/lib/shadow-powerd/schedule.state (key = value). Day counts use wall-clock time,
//! so a week means a calendar week whether or not the laptop was in use every day.
use crate::sys;

pub const LEARN_DAYS: u64 = 7;
pub const DRIFT_DAYS: u64 = 2;
const DAY: u64 = 86_400;
const ALPHA_LEARNING: f64 = 0.30;
const ALPHA_STABLE: f64 = 0.05;
const DRIFT_ERR: f64 = 0.25;
const DRIFT_MIN_SAMPLES: u32 = 40;

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Learning { since: u64, days: u64, reason: String },
    Learned { since: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Schedule {
    pub phase: Phase,
    /// what the OS looked like when learning last (re)started: kernel + last full upgrade
    pub os: String,
    err: f64,
    err_n: u32,
}

impl Schedule {
    pub fn new(now: u64, os: &str) -> Schedule {
        Schedule { phase: Phase::Learning { since: now, days: LEARN_DAYS, reason: "first-week".into() }, os: os.to_string(), err: 0.0, err_n: 0 }
    }

    pub fn relearn(&mut self, now: u64, days: u64, reason: &str) {
        self.phase = Phase::Learning { since: now, days, reason: reason.into() };
        self.err = 0.0;
        self.err_n = 0;
    }

    /// Advance the schedule. Returns a log line when the phase changes.
    pub fn tick(&mut self, now: u64, os: &str) -> Option<String> {
        if !os.is_empty() && !self.os.is_empty() && os != self.os {
            let old = std::mem::replace(&mut self.os, os.to_string());
            self.relearn(now, LEARN_DAYS, "os-update");
            return Some(format!("learning: OS changed ({old} → {os}), relearning for {LEARN_DAYS} days"));
        }
        if self.os.is_empty() {
            self.os = os.to_string();
        }
        if let Phase::Learning { since, days, reason } = &self.phase {
            if now >= since + days * DAY {
                let msg = format!("learning: {reason} phase complete after {days} days — predictions now drive power settings");
                self.phase = Phase::Learned { since: now };
                return Some(msg);
            }
        }
        None
    }

    /// Record how far the prediction for a class was from what it then used (0..1).
    pub fn prediction_error(&mut self, now: u64, err: f64) -> Option<String> {
        if !matches!(self.phase, Phase::Learned { .. }) {
            return None;
        }
        self.err = if self.err_n == 0 { err } else { 0.9 * self.err + 0.1 * err };
        self.err_n = self.err_n.saturating_add(1);
        if self.err_n >= DRIFT_MIN_SAMPLES && self.err > DRIFT_ERR {
            let e = self.err;
            self.relearn(now, DRIFT_DAYS, "drift");
            return Some(format!("learning: predictions drifted (error {:.0}%), relearning for {DRIFT_DAYS} days", e * 100.0));
        }
        None
    }

    pub fn feed_forward(&self) -> bool {
        matches!(self.phase, Phase::Learned { .. })
    }

    /// learning rate for a class with `n` samples so far
    #[cfg(test)]
    pub fn alpha(&self, n: u32) -> f64 {
        alpha(n, self.feed_forward())
    }

    /// (day of learning 1..=days, days, reason) while learning
    pub fn progress(&self, now: u64) -> Option<(u64, u64, String)> {
        match &self.phase {
            Phase::Learning { since, days, reason } => Some((((now.saturating_sub(*since)) / DAY + 1).min(*days), *days, reason.clone())),
            _ => None,
        }
    }

    pub fn save(&self) -> String {
        let mut t = String::from("# shadow-powerd learning schedule\n");
        match &self.phase {
            Phase::Learning { since, days, reason } => t.push_str(&format!("phase = learning\nsince = {since}\ndays = {days}\nreason = {reason}\n")),
            Phase::Learned { since } => t.push_str(&format!("phase = learned\nsince = {since}\n")),
        }
        t.push_str(&format!("os = {}\nerr = {:.4}\nerr_n = {}\n", self.os, self.err, self.err_n));
        t
    }

    pub fn load(text: &str, now: u64, os: &str) -> Schedule {
        let kv: Vec<(String, String)> = text.lines().filter(|l| !l.trim_start().starts_with('#')).filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect();
        let get = |k: &str| kv.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        let since = get("since").and_then(|v| v.parse().ok()).unwrap_or(now);
        let phase = match get("phase").as_deref() {
            Some("learned") => Phase::Learned { since },
            Some("learning") => Phase::Learning { since, days: get("days").and_then(|v| v.parse().ok()).unwrap_or(LEARN_DAYS), reason: get("reason").unwrap_or_else(|| "first-week".into()) },
            _ => return Schedule::new(now, os),
        };
        Schedule { phase, os: get("os").unwrap_or_default(), err: get("err").and_then(|v| v.parse().ok()).unwrap_or(0.0), err_n: get("err_n").and_then(|v| v.parse().ok()).unwrap_or(0) }
    }
}

/// Fingerprint of the installed OS: running kernel + time of the last completed full upgrade.
/// Installing a single package is not an OS update; `pacman -Syu` and kernel changes are.
/// EWMA rate: fast while learning or for classes with few samples, slow once learned.
pub fn alpha(n: u32, learned: bool) -> f64 {
    if n < 10 || !learned {
        ALPHA_LEARNING
    } else {
        ALPHA_STABLE
    }
}

pub fn os_fingerprint() -> String {
    let kernel = sys::read("proc/sys/kernel/osrelease").map(|s| s.trim().to_string()).unwrap_or_default();
    let upgrade = if crate::distro::family() == crate::distro::Family::Rpm {
        let (code, out, _) = sys::run(&["rpm", "-qa", "--queryformat", "%{INSTALLTIME}\n"]);
        if code == 0 { last_bulk_install(&out) } else { None }
    } else {
        last_full_upgrade(&sys::read("var/log/pacman.log").unwrap_or_default())
    };
    format!("{kernel}@{}", upgrade.unwrap_or_default())
}

/// The file whose change means "packages changed": pacman's log, or the rpm database.
pub fn package_db_marker() -> std::path::PathBuf {
    for rel in ["var/log/pacman.log", "var/lib/rpm/rpmdb.sqlite", "usr/lib/sysimage/rpm/rpmdb.sqlite", "var/lib/rpm/Packages"] {
        let p = sys::p(rel);
        if p.exists() {
            return p;
        }
    }
    sys::p("var/log/pacman.log")
}

/// RPM systems (dnf upgrade, dnf5, RHEL): an OS update is a burst of 5+ packages installed
/// within 15 minutes of each other; the time of the most recent such burst. A single
/// `dnf install foo` doesn't count.
pub fn last_bulk_install(install_times: &str) -> Option<String> {
    let mut t: Vec<u64> = install_times.lines().filter_map(|l| l.trim().parse().ok()).collect();
    t.sort_unstable();
    let mut best = None;
    let mut i = 0;
    while i < t.len() {
        let mut j = i;
        while j + 1 < t.len() && t[j + 1] - t[j] <= 900 {
            j += 1;
        }
        if j + 1 - i >= 5 {
            best = Some(t[i].to_string());
        }
        i = j + 1;
    }
    best
}

/// Timestamp text of the last `pacman -Syu` that actually upgraded something.
pub fn last_full_upgrade(log: &str) -> Option<String> {
    let mut current: Option<String> = None;
    let mut last: Option<String> = None;
    let mut upgraded = false;
    for l in log.lines() {
        let ts = l.strip_prefix('[').and_then(|r| r.split_once(']')).map(|(t, _)| t.to_string());
        if l.contains("[PACMAN] starting full system upgrade") {
            current = ts;
            upgraded = false;
        } else if l.contains("[ALPM] upgraded ") && current.is_some() {
            upgraded = true;
        } else if l.contains("[ALPM] transaction completed") {
            if upgraded {
                last = current.clone();
            }
            current = None;
            upgraded = false;
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_800_000_000;

    #[test]
    fn first_week_then_learned() {
        let mut s = Schedule::new(T0, "6.12.1@A");
        assert!(!s.feed_forward());
        assert_eq!(s.progress(T0 + 3 * DAY + 10), Some((4, 7, "first-week".into())));
        assert!(s.tick(T0 + 6 * DAY, "6.12.1@A").is_none());
        assert!(s.tick(T0 + 7 * DAY, "6.12.1@A").is_some());
        assert!(s.feed_forward());
        assert_eq!(s.alpha(50), ALPHA_STABLE);
        assert_eq!(s.alpha(3), ALPHA_LEARNING, "new apps still learn fast");
    }

    #[test]
    fn os_update_relearns_a_week() {
        let mut s = Schedule::new(T0, "6.12.1@A");
        s.tick(T0 + 8 * DAY, "6.12.1@A");
        assert!(s.feed_forward());
        let msg = s.tick(T0 + 20 * DAY, "6.12.4@B").unwrap();
        assert!(msg.contains("relearning for 7 days"));
        assert!(!s.feed_forward());
        assert_eq!(s.progress(T0 + 20 * DAY).unwrap().2, "os-update");
        s.tick(T0 + 27 * DAY, "6.12.4@B");
        assert!(s.feed_forward());
    }

    #[test]
    fn drift_relearns_briefly() {
        let mut s = Schedule::new(T0, "k@A");
        s.tick(T0 + 7 * DAY, "k@A");
        for _ in 0..39 {
            assert!(s.prediction_error(T0 + 8 * DAY, 0.5).is_none());
        }
        assert!(s.prediction_error(T0 + 8 * DAY, 0.5).is_some());
        assert_eq!(s.progress(T0 + 8 * DAY).unwrap(), (1, DRIFT_DAYS, "drift".into()));
        // good predictions never trigger it
        let mut g = Schedule::new(T0, "k@A");
        g.tick(T0 + 7 * DAY, "k@A");
        for _ in 0..500 {
            assert!(g.prediction_error(T0 + 8 * DAY, 0.05).is_none());
        }
    }

    #[test]
    fn persists() {
        let mut s = Schedule::new(T0, "k@A");
        s.tick(T0 + 7 * DAY, "k@A");
        let l = Schedule::load(&s.save(), T0 + 9 * DAY, "k@A");
        assert_eq!(l, s);
        assert_eq!(Schedule::load("", T0, "k@A").progress(T0).unwrap().2, "first-week");
    }

    #[test]
    fn rpm_bursts() {
        // one dnf upgrade of 6 packages, then a single install later
        let t = "1800000000\n1800000030\n1800000045\n1800000100\n1800000200\n1800000300\n1800090000\n";
        assert_eq!(last_bulk_install(t).as_deref(), Some("1800000000"));
        assert_eq!(last_bulk_install("1800000000\n1800090000\n"), None);
    }

    #[test]
    fn pacman_log() {
        let log = "[2026-10-01T10:00:00-0600] [PACMAN] Running 'pacman -S htop'\n\
[2026-10-01T10:00:01-0600] [ALPM] transaction started\n\
[2026-10-01T10:00:02-0600] [ALPM] installed htop (3.4-1)\n\
[2026-10-01T10:00:03-0600] [ALPM] transaction completed\n\
[2026-10-20T09:00:00-0600] [PACMAN] starting full system upgrade\n\
[2026-10-20T09:00:00-0600] [PACMAN] starting full system upgrade\n\
[2026-10-20T09:01:00-0600] [ALPM] upgraded mesa (25.2.1-1 -> 25.2.3-1)\n\
[2026-10-20T09:02:00-0600] [ALPM] transaction completed\n\
[2026-10-25T09:00:00-0600] [PACMAN] starting full system upgrade\n";
        // the last -Syu found nothing to do (no transaction) — the previous one counts
        assert_eq!(last_full_upgrade(log).as_deref(), Some("2026-10-20T09:00:00-0600"));
        assert_eq!(last_full_upgrade("[x] [ALPM] installed a\n[x] [ALPM] transaction completed\n"), None);
    }
}
