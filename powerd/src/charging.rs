//! Smart charging (Dell Optimizer-style) without risking the firmware.
//!
//! Every charging change is a write into the BIOS's non-volatile settings (Dell: an SMBIOS token
//! through the dell-laptop driver). So:
//! * only modes the firmware itself lists in `charge_types` are ever written, and thresholds only
//!   when it offers Custom, inside Dell's own limits (start 50–95, end 55–100, ≥ 5 apart);
//! * nothing is written when the requested setting is already in effect;
//! * every write is read back; a mismatch stops smart charging until you choose a mode again;
//! * at most MAX_WRITES_PER_DAY charging writes in any 24 hours (manual ones included);
//! * the smart choice is re-evaluated at most once a day and only written when it changes.
//!
//! Smart: from how much of the last 7 days the laptop spent on AC —
//!   mostly plugged in (≥ 80 %) → hold the battery around 60–80 % (Custom), or Dell's "Primarily
//!                                AC" (Trickle) if Custom isn't offered
//!   mixed use                  → the firmware's own Adaptive mode if offered, else Standard
//! "Charge to 100 % once" (travel): Standard until the battery is full (or 24 h), then back to smart.
use crate::power;
use crate::sys;
use std::path::PathBuf;

pub const MAX_WRITES_PER_DAY: usize = 6;
const DAY: u64 = 86400;

#[derive(Default, Debug, PartialEq)]
pub struct Guard {
    /// timestamps of charging writes in the last 24 h
    pub writes: Vec<u64>,
    /// (day number, seconds on AC, seconds observed) for the last 7 days
    pub days: Vec<(u64, u64, u64)>,
    pub last_smart: u64,
    /// "charge to 100 % once" started at this time
    pub full_once: Option<u64>,
    /// a write didn't read back as requested: smart charging paused
    pub paused: bool,
}

fn path() -> PathBuf {
    sys::p("var/lib/shadow-powerd/charge.state")
}

impl Guard {
    pub fn load() -> Guard {
        Guard::parse(&std::fs::read_to_string(path()).unwrap_or_default())
    }

    pub fn parse(t: &str) -> Guard {
        let mut g = Guard::default();
        for l in t.lines() {
            let Some((k, v)) = l.split_once('=') else { continue };
            match k.trim() {
                "write" => g.writes.extend(v.trim().parse::<u64>().ok()),
                "day" => {
                    let f: Vec<u64> = v.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                    if f.len() == 3 {
                        g.days.push((f[0], f[1], f[2]));
                    }
                }
                "last_smart" => g.last_smart = v.trim().parse().unwrap_or(0),
                "full_once" => g.full_once = v.trim().parse().ok(),
                "paused" => g.paused = v.trim() == "1",
                _ => {}
            }
        }
        g
    }

    pub fn save(&self) {
        let mut t = String::from("# shadow-powerd charging guard (write budget, AC history)\n");
        for w in &self.writes {
            t.push_str(&format!("write={w}\n"));
        }
        for (d, a, o) in &self.days {
            t.push_str(&format!("day={d} {a} {o}\n"));
        }
        t.push_str(&format!("last_smart={}\npaused={}\n", self.last_smart, self.paused as u8));
        if let Some(f) = self.full_once {
            t.push_str(&format!("full_once={f}\n"));
        }
        let _ = std::fs::create_dir_all(path().parent().unwrap());
        let _ = sys::atomic_write(&path(), &t);
    }

    /// Account `secs` of observed time, on AC or not.
    pub fn sample(&mut self, ac: bool, secs: u64, now: u64) {
        let day = now / DAY;
        match self.days.last_mut() {
            Some(d) if d.0 == day => {
                d.2 += secs;
                if ac {
                    d.1 += secs;
                }
            }
            _ => self.days.push((day, if ac { secs } else { 0 }, secs)),
        }
        self.days.retain(|d| d.0 + 7 > day);
    }

    /// Share of observed time on AC over the last 7 days (needs ≥ 2 days and ≥ 4 h observed).
    pub fn ac_share(&self) -> Option<f64> {
        let (a, o) = self.days.iter().fold((0u64, 0u64), |s, d| (s.0 + d.1, s.1 + d.2));
        (self.days.len() >= 2 && o >= 4 * 3600).then(|| a as f64 / o as f64)
    }

    pub fn budget_left(&mut self, now: u64) -> usize {
        self.writes.retain(|w| now.saturating_sub(*w) < DAY);
        MAX_WRITES_PER_DAY.saturating_sub(self.writes.len())
    }
}

/// The smart decision: (mode, start, end)
pub fn smart_choice(available: &[String], thresholds: bool, share: Option<f64>) -> (&'static str, Option<u8>, Option<u8>) {
    let has = |t: &str| available.iter().any(|a| a == t);
    match share {
        Some(s) if s >= 0.8 => {
            if has("Custom") && thresholds {
                ("custom", Some(60), Some(80))
            } else if has("Trickle") {
                ("ac", None, None)
            } else if has("Adaptive") {
                ("adaptive", None, None)
            } else {
                ("standard", None, None)
            }
        }
        _ => {
            if has("Adaptive") {
                ("adaptive", None, None)
            } else {
                ("standard", None, None)
            }
        }
    }
}

/// Write a charging mode the safe way. Ok(true) = written, Ok(false) = already in effect.
pub fn safe_set(h: &crate::hw::Hw, g: &mut Guard, mode: &str, start: Option<u8>, end: Option<u8>, now: u64) -> Result<bool, String> {
    let tok = power::charge_token(mode, &h.charge_types).ok_or_else(|| format!("charge mode '{mode}' is not offered by this firmware (available: {})", if h.charge_types.is_empty() { "none".into() } else { h.charge_types.join(" ") }))?;
    let cur = power::charge_current(h);
    let same_thresholds = mode != "custom" || power::thresholds(h) == start.zip(end);
    if cur.as_deref() == Some(tok.as_str()) && same_thresholds {
        return Ok(false);
    }
    if g.budget_left(now) == 0 {
        return Err(format!("charging was changed {MAX_WRITES_PER_DAY} times in the last 24 h — limit reached to protect the firmware's settings memory; try again later"));
    }
    g.writes.push(now);
    power::set_charge(h, mode, start, end)?;
    // read back: the firmware must report exactly what we asked for
    let after = power::charge_current(h);
    if after.as_deref() != Some(tok.as_str()) {
        g.paused = true;
        return Err(format!("the firmware reports '{}' after writing '{tok}': smart charging paused, nothing more is written", after.unwrap_or_else(|| "?".into())));
    }
    if mode == "custom" && power::thresholds(h) != start.zip(end) {
        g.paused = true;
        return Err("the firmware did not keep the requested thresholds: smart charging paused".into());
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_follows_ac_share() {
        let dell: Vec<String> = ["Trickle", "Fast", "Standard", "Adaptive", "Custom"].map(String::from).to_vec();
        assert_eq!(smart_choice(&dell, true, Some(0.9)), ("custom", Some(60), Some(80)));
        assert_eq!(smart_choice(&dell, false, Some(0.9)), ("ac", None, None));
        assert_eq!(smart_choice(&dell, true, Some(0.4)), ("adaptive", None, None));
        assert_eq!(smart_choice(&dell, true, None), ("adaptive", None, None), "no history yet: the firmware's own adaptive mode");
        let plain: Vec<String> = vec!["Standard".into()];
        assert_eq!(smart_choice(&plain, false, Some(0.95)), ("standard", None, None), "never a mode the firmware doesn't list");
    }

    #[test]
    fn history_and_budget() {
        let mut g = Guard::default();
        let t0 = 100 * DAY;
        g.sample(true, 3 * 3600, t0);
        g.sample(false, 3600, t0 + DAY);
        g.sample(true, 4 * 3600, t0 + DAY);
        assert_eq!(g.ac_share(), Some(7.0 / 8.0));
        g.sample(true, 60, t0 + 9 * DAY);
        assert_eq!(g.days.len(), 1, "only the last 7 days are kept");
        g.writes = vec![t0; MAX_WRITES_PER_DAY];
        assert_eq!(g.budget_left(t0 + 10), 0);
        assert_eq!(g.budget_left(t0 + DAY + 1), MAX_WRITES_PER_DAY);
        let s = Guard { writes: vec![5], days: vec![(1, 2, 3)], last_smart: 9, full_once: Some(7), paused: true };
        let mut t = String::new();
        for w in &s.writes { t.push_str(&format!("write={w}\n")); }
        t.push_str("day=1 2 3\nlast_smart=9\npaused=1\nfull_once=7\n");
        assert_eq!(Guard::parse(&t), s);
    }

    #[test]
    fn writes_only_what_the_firmware_offers_and_verifies() {
        let r = std::env::temp_dir().join(format!("chg-{}", std::process::id()));
        let b = r.join("sys/class/power_supply/BAT0");
        std::fs::create_dir_all(&b).unwrap();
        // a fake driver: charge_types lists modes, the active one in brackets; writes replace it
        std::fs::write(b.join("charge_types"), "Trickle Fast [Standard] Adaptive Custom\n").unwrap();
        std::fs::write(b.join("charge_control_start_threshold"), "50\n").unwrap();
        std::fs::write(b.join("charge_control_end_threshold"), "90\n").unwrap();
        let _env = crate::sys::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SHADOW_OPT_ROOT", &r);
        let mut h = crate::hw::Hw::default();
        h.battery = Some("BAT0".into());
        h.charge_types = ["Trickle", "Fast", "Standard", "Adaptive", "Custom"].map(String::from).to_vec();
        h.charge_thresholds = true;
        let mut g = Guard::default();
        // already Standard: nothing written
        assert_eq!(safe_set(&h, &mut g, "standard", None, None, 1000), Ok(false));
        assert!(g.writes.is_empty());
        // a mode the firmware doesn't list is refused before anything is written
        h.charge_types.retain(|t| t != "Adaptive");
        assert!(safe_set(&h, &mut g, "adaptive", None, None, 1000).is_err());
        assert!(g.writes.is_empty());
        // Custom 60–80: thresholds written, then the mode; the fake file then holds the bare word
        // "Custom" (a real driver shows the list with brackets) — both read back as current
        assert_eq!(safe_set(&h, &mut g, "custom", Some(60), Some(80), 1000), Ok(true));
        assert_eq!(std::fs::read_to_string(b.join("charge_control_end_threshold")).unwrap().trim(), "80");
        assert_eq!(g.writes.len(), 1);
        std::env::remove_var("SHADOW_OPT_ROOT");
        let _ = std::fs::remove_dir_all(&r);
    }
}
