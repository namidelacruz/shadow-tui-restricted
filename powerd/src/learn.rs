//! Adaptive learning from what the user actually does.
//!
//! Every manual choice (thermal mode, refresh rate) is recorded with its context: power source,
//! battery level, focused app, time of day. `auto` then follows a learned preference when there
//! is enough evidence (>= 3 weighted observations, >= 70 % agreement), backing off from the
//! exact context to coarser ones. Old evidence fades (half-life 14 days), so habits can change.
//!
//! This runs on the CPU in microseconds. The NPU is reserved for the model phases where it pays
//! off (local LLM, larger usage models); `shadow-powerd npu` reports whether it is ready.
use crate::sys;
use std::collections::BTreeMap;
use std::path::PathBuf;

const HALF_LIFE_DAYS: f64 = 14.0;
pub const MIN_EVIDENCE: f64 = 2.9; // "three recent choices" (fresh observations weigh just under 1.0)
pub const MIN_SHARE: f64 = 0.7;

#[derive(Debug, Clone)]
pub struct Ctx {
    pub ac: bool,
    pub battery_pct: Option<u8>,
    pub app: String,
    pub hour: u32,
}

impl Ctx {
    fn bat(&self) -> &'static str {
        match self.battery_pct {
            None => "na",
            Some(p) if p < 20 => "low",
            Some(p) if p < 50 => "mid",
            _ => "high",
        }
    }
    fn day(&self) -> &'static str {
        match self.hour {
            5..=11 => "morning",
            12..=16 => "afternoon",
            17..=22 => "evening",
            _ => "night",
        }
    }
    /// keys from most to least specific, with a readable description
    pub fn levels(&self) -> Vec<(String, String)> {
        let src = if self.ac { "ac" } else { "battery" };
        let app = if self.app.is_empty() { "-" } else { &self.app };
        vec![
            (format!("{src}|{}|{app}|{}", self.bat(), self.day()),
             format!("on {src}, battery {}, using {app}, {}", self.bat(), self.day())),
            (format!("{src}|*|{app}|*"), format!("on {src}, using {app}")),
            (format!("{src}|{}|*|*", self.bat()), format!("on {src}, battery {}", self.bat())),
            (format!("{src}|*|*|*"), format!("on {src}")),
        ]
    }
    pub fn full_key(&self) -> String {
        self.levels()[0].0.clone()
    }
}

pub fn events_path() -> PathBuf {
    sys::state_dir().join("opt-events.tsv")
}

pub fn record(kind: &str, ctx: &Ctx, choice: &str) {
    use std::io::Write;
    let p = events_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{}\t{kind}\t{}\t{choice}", sys::now(), ctx.full_key());
    }
}

/// Does a stored full key match a (possibly wildcarded) level key?
fn matches(stored: &str, pattern: &str) -> bool {
    let a: Vec<&str> = stored.split('|').collect();
    let b: Vec<&str> = pattern.split('|').collect();
    a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| *y == "*" || x == y)
}

pub struct Model {
    // (kind, stored key) -> choice -> weight
    obs: Vec<(String, String, String, f64)>,
}

impl Model {
    pub fn load() -> Model {
        let now = sys::now() as f64;
        let mut obs = vec![];
        let text = std::fs::read_to_string(events_path()).unwrap_or_default();
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() != 4 {
                continue;
            }
            let Ok(ts) = f[0].parse::<f64>() else { continue };
            let age_days = ((now - ts) / 86400.0).max(0.0);
            let w = 0.5f64.powf(age_days / HALF_LIFE_DAYS);
            obs.push((f[1].to_string(), f[2].to_string(), f[3].to_string(), w));
        }
        Model { obs }
    }

    /// (choice, share, evidence, description of the context level used)
    pub fn predict(&self, kind: &str, ctx: &Ctx) -> Option<(String, f64, f64, String)> {
        for (pat, desc) in ctx.levels() {
            let mut tally: BTreeMap<&str, f64> = BTreeMap::new();
            let mut apps: Vec<&str> = vec![];
            for (k, key, choice, w) in &self.obs {
                if k == kind && matches(key, &pat) {
                    *tally.entry(choice).or_default() += w;
                    let app = key.split('|').nth(2).unwrap_or("-");
                    if !apps.contains(&app) {
                        apps.push(app);
                    }
                }
            }
            let total: f64 = tally.values().sum();
            if total < MIN_EVIDENCE {
                continue; // not enough evidence at this level: back off
            }
            // a rule for "any app" needs evidence from more than one app, or one app's habit
            // would be applied to everything
            if pat.split('|').nth(2) == Some("*") && apps.len() < 2 && apps != ["-"] {
                continue;
            }
            let (best, w) = tally.iter().max_by(|a, b| a.1.total_cmp(b.1))?;
            let share = w / total;
            return (share >= MIN_SHARE).then(|| (best.to_string(), share, total, desc));
        }
        None
    }

    /// Human-readable list of what has been learned.
    pub fn explain(&self) -> Vec<String> {
        let mut groups: BTreeMap<(String, String), BTreeMap<String, f64>> = BTreeMap::new();
        for (k, key, choice, w) in &self.obs {
            let parts: Vec<&str> = key.split('|').collect();
            if parts.len() != 4 {
                continue;
            }
            // summarise by source + app, the levels people recognise
            let g = format!("{} · {}", parts[0], if parts[2] == "-" { "any app" } else { parts[2] });
            *groups.entry((k.clone(), g)).or_default().entry(choice.clone()).or_default() += w;
        }
        let mut out = vec![];
        for ((kind, g), tally) in groups {
            let total: f64 = tally.values().sum();
            let (best, w) = tally.iter().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
            let state = if total >= MIN_EVIDENCE && w / total >= MIN_SHARE { "learned" } else { "watching" };
            out.push(format!("{kind:<8} {g:<32} → {best:<12} {:>3.0}% of {total:.1} obs  [{state}]", 100.0 * w / total));
        }
        out
    }

    pub fn len(&self) -> usize {
        self.obs.len()
    }
}

pub fn reset() -> std::io::Result<()> {
    match std::fs::remove_file(events_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wildcard_matching() {
        assert!(matches("ac|high|code|evening", "ac|*|code|*"));
        assert!(!matches("battery|high|code|evening", "ac|*|*|*"));
    }
    #[test]
    fn backs_off_and_needs_agreement() {
        let ctx = Ctx { ac: false, battery_pct: Some(80), app: "code".into(), hour: 20 };
        let key = ctx.full_key();
        let mut m = Model { obs: vec![] };
        for _ in 0..2 {
            m.obs.push(("profile".into(), key.clone(), "performance".into(), 1.0));
        }
        assert!(m.predict("profile", &ctx).is_none(), "2 observations are not enough");
        m.obs.push(("profile".into(), key.clone(), "performance".into(), 1.0));
        assert_eq!(m.predict("profile", &ctx).unwrap().0, "performance");
        // disagreement drops below 70 %
        m.obs.push(("profile".into(), key.clone(), "quiet".into(), 1.0));
        m.obs.push(("profile".into(), key.clone(), "quiet".into(), 1.0));
        assert!(m.predict("profile", &ctx).is_none());
    }
    #[test]
    fn one_apps_habit_is_not_applied_to_all_apps() {
        let mut m = Model { obs: vec![] };
        for _ in 0..3 {
            let c = Ctx { ac: false, battery_pct: Some(80), app: "code".into(), hour: 20 };
            m.obs.push(("refresh".into(), c.full_key(), "max".into(), 1.0));
        }
        let other = Ctx { ac: false, battery_pct: Some(80), app: "firefox".into(), hour: 20 };
        assert!(m.predict("refresh", &other).is_none());
        let same = Ctx { ac: false, battery_pct: Some(40), app: "code".into(), hour: 9 };
        assert_eq!(m.predict("refresh", &same).unwrap().0, "max", "same app, other battery/time: app level applies");
    }
    #[test]
    fn coarser_level_used_for_new_app() {
        let mut m = Model { obs: vec![] };
        for app in ["a", "b", "c"] {
            let c = Ctx { ac: false, battery_pct: Some(30), app: app.into(), hour: 9 };
            m.obs.push(("profile".into(), c.full_key(), "quiet".into(), 1.0));
        }
        let new = Ctx { ac: false, battery_pct: Some(35), app: "new".into(), hour: 9 };
        let (choice, _, _, desc) = m.predict("profile", &new).unwrap();
        assert_eq!(choice, "quiet");
        assert!(desc.contains("battery mid"));
    }
}
