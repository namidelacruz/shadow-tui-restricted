//! Applications for the launcher: desktop entries, fuzzy matching, and intent ranking.
//!
//! Ranking blends three learned signals, all local:
//!  * how often an app is launched (history),
//!  * how often around this hour of the day (24-slot histogram),
//!  * what usually follows the app that is focused now (a Markov transition table,
//!    kept in npu_weights-adjacent state: ~/.config/shadow-state/launcher.markov).
//! RAM / power estimates come from what the session has measured for that app before:
//! resident memory sampled by shadow-sessiond, CPU share learned by shadow-powerd.
use crate::{atomic_write, home, state_dir};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub detail: String,
    pub tag: &'static str,
    pub cmd: String,
    pub terminal: bool,
    pub keywords: String,
    pub cats: String,
    /// lower-case app ids this entry's windows are likely to use
    pub app_ids: Vec<String>,
}

pub fn data_dirs() -> Vec<PathBuf> {
    let mut v = vec![PathBuf::from(std::env::var("XDG_DATA_HOME").unwrap_or_else(|_| home().join(".local/share").to_string_lossy().to_string()))];
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let sys = if sys.is_empty() { "/usr/local/share:/usr/share".to_string() } else { sys };
    v.extend(sys.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    v.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    v.push(home().join(".local/share/flatpak/exports/share"));
    v
}

pub fn strip_field_codes(exec: &str) -> String {
    let mut out = String::new();
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('%') => out.push('%'),
                Some(_) | None => {}
            }
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_desktop(path: &PathBuf, id: &str) -> Option<Entry> {
    let text = fs::read_to_string(path).ok()?;
    let mut in_main = false;
    let mut kv: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_main = l == "[Desktop Entry]";
            continue;
        }
        if !in_main || l.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            kv.entry(k.trim().to_string()).or_insert_with(|| v.trim().to_string());
        }
    }
    let is_true = |k: &str| kv.get(k).map(|v| v == "true").unwrap_or(false);
    if kv.get("Type").map(|t| t != "Application").unwrap_or(true) || is_true("NoDisplay") || is_true("Hidden") {
        return None;
    }
    let exec = strip_field_codes(kv.get("Exec")?);
    if exec.is_empty() {
        return None;
    }
    Some(Entry {
        id: format!("app:{id}"),
        name: kv.get("Name")?.clone(),
        detail: kv.get("GenericName").or_else(|| kv.get("Comment")).cloned().unwrap_or_default(),
        tag: "app",
        cmd: exec.clone(),
        terminal: is_true("Terminal"),
        keywords: kv.get("Keywords").cloned().unwrap_or_default(),
        cats: kv.get("Categories").cloned().unwrap_or_default(),
        app_ids: {
            let mut v = vec![id.trim_end_matches(".desktop").to_lowercase()];
            if let Some(w) = kv.get("StartupWMClass") {
                v.push(w.to_lowercase());
            }
            if let Some(bin) = exec.split_whitespace().next() {
                v.push(bin.rsplit('/').next().unwrap_or(bin).to_lowercase());
            }
            v
        },
    })
}

pub fn load_apps() -> Vec<Entry> {
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut out = vec![];
    for d in data_dirs() {
        let dir = d.join("applications");
        let mut stack = vec![dir.clone()];
        while let Some(cur) = stack.pop() {
            let Ok(rd) = fs::read_dir(&cur) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                if p.extension().map(|x| x != "desktop").unwrap_or(true) {
                    continue;
                }
                let id = p.strip_prefix(&dir).map(|r| r.to_string_lossy().replace('/', "-")).unwrap_or_default();
                if seen.contains_key(&id) {
                    continue; // earlier dirs (user) win
                }
                seen.insert(id.clone(), ());
                if let Some(en) = parse_desktop(&p, &id) {
                    out.push(en);
                }
            }
        }
    }
    out
}

/// Extra weight when the query matches the app's own name: exactly, at its start, or at the start
/// of one of its words. Without it a long description that happens to contain the letters
/// ("Yazi (foot)", a file manager that runs in foot) can outrank the app you typed ("Foot").
pub fn name_bonus(q: &str, name: &str) -> i32 {
    let (q, n) = (q.trim().to_lowercase(), name.to_lowercase());
    if q.is_empty() {
        0
    } else if n == q {
        400
    } else if n.starts_with(&q) {
        200
    } else if n.split(|c: char| !c.is_alphanumeric()).any(|w| w.starts_with(&q)) {
        100
    } else {
        0
    }
}

/// Subsequence fuzzy score; higher is better. None = no match.
pub fn score(q: &str, hay: &str) -> Option<i32> {
    if q.is_empty() {
        return Some(0);
    }
    let h: Vec<char> = hay.to_lowercase().chars().collect();
    let mut s = 0;
    let mut hi = 0;
    let mut last: Option<usize> = None;
    for qc in q.to_lowercase().chars() {
        let mut found = false;
        while hi < h.len() {
            if h[hi] == qc {
                s += 10;
                if last.map(|l| l + 1 == hi).unwrap_or(false) {
                    s += 15; // consecutive
                }
                if hi == 0 || !h[hi - 1].is_alphanumeric() {
                    s += 20; // word start
                }
                last = Some(hi);
                hi += 1;
                found = true;
                break;
            }
            hi += 1;
        }
        if !found {
            return None;
        }
    }
    Some(s - (h.len() as i32 / 4))
}

fn history_path() -> PathBuf {
    state_dir().join("launcher.hist")
}

pub fn load_history() -> HashMap<String, i32> {
    fs::read_to_string(history_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t').and_then(|(k, v)| v.parse().ok().map(|n| (k.to_string(), n))))
        .collect()
}

fn hours_path() -> PathBuf {
    state_dir().join("launcher.hours")
}

pub fn now_hour() -> usize {
    crate::local_hour()
}

/// Per-app launch counts by hour of day: "id\tc0,c1,…,c23"
pub fn load_hours() -> HashMap<String, [u32; 24]> {
    fs::read_to_string(hours_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('\t')?;
            let mut a = [0u32; 24];
            for (i, x) in v.split(',').take(24).enumerate() {
                a[i] = x.parse().unwrap_or(0);
            }
            Some((k.to_string(), a))
        })
        .collect()
}

/// How much this app is used around this time of day (±1 hour), for ranking.
pub fn hour_score(h: &HashMap<String, [u32; 24]>, id: &str, hour: usize) -> i32 {
    h.get(id).map_or(0, |a| (a[(hour + 23) % 24] + 2 * a[hour] + a[(hour + 1) % 24]) as i32)
}

fn bump_hours(id: &str) {
    let mut h = load_hours();
    let e = h.entry(id.to_string()).or_insert([0; 24]);
    e[now_hour()] += 1;
    let text: String = h.iter().map(|(k, a)| format!("{k}\t{}\n", a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","))).collect();
    let _ = atomic_write(&hours_path(), &text);
}

fn bump_history_inner(id: &str) {
    bump_hours(id);
    let mut h = load_history();
    *h.entry(id.to_string()).or_insert(0) += 1;
    let mut v: Vec<_> = h.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(300);
    let text: String = v.iter().map(|(k, n)| format!("{k}\t{n}\n")).collect();
    let _ = atomic_write(&history_path(), &text);
}


fn markov_path() -> PathBuf {
    state_dir().join("launcher.markov")
}

/// "from\tto\tcount"
pub fn load_markov() -> HashMap<(String, String), u32> {
    fs::read_to_string(markov_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some(((f.next()?.to_string(), f.next()?.to_string()), f.next()?.parse().ok()?))
        })
        .collect()
}

/// P(next = id | focused = from) scaled to 0..100
pub fn markov_score(m: &HashMap<(String, String), u32>, from: &str, id: &str) -> i32 {
    if from.is_empty() {
        return 0;
    }
    let total: u32 = m.iter().filter(|((f, _), _)| f == from).map(|(_, n)| *n).sum();
    if total == 0 {
        return 0;
    }
    (100 * m.get(&(from.to_string(), id.to_string())).copied().unwrap_or(0) / total) as i32
}

/// Record a launch: history, hour of day, and the transition from the focused app.
pub fn record_launch(id: &str, from: &str) {
    bump_history_inner(id);
    if from.is_empty() {
        return;
    }
    let mut m = load_markov();
    *m.entry((from.to_string(), id.to_string())).or_insert(0) += 1;
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.truncate(2000);
    let text: String = v.iter().map(|((f, t), n)| format!("{f}\t{t}\t{n}\n")).collect();
    let _ = atomic_write(&markov_path(), &text);
}

/// Measured resident memory per app id (MB), from shadow-sessiond: "app_id\tmb"
pub fn load_ram() -> HashMap<String, u32> {
    fs::read_to_string(state_dir().join("apps.ram"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t').and_then(|(k, v)| v.trim().parse().ok().map(|n| (k.to_lowercase(), n))))
        .collect()
}

/// FNV-1a, the class id used by the telemetry records
pub fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

/// Learned CPU share per class from shadow-powerd: "class util samples"
pub fn load_cpu() -> HashMap<u32, f64> {
    let p = std::env::var("SHADOW_POWERD_LEARN").unwrap_or_else(|_| "/var/lib/shadow-powerd/learn.state".into());
    fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() == 3 && f[2].parse::<u32>().ok()? >= 3).then(|| Some((f[0].parse().ok()?, f[1].parse().ok()?)))?
        })
        .collect()
}

/// Rough package power while the app is in front: idle floor + its learned CPU share of a
/// Lunar Lake package budget (17 W PL1). An estimate, shown with "~".
pub fn watts_from_util(u: f64) -> f64 {
    2.0 + u.clamp(0.0, 1.0) * 15.0
}

pub fn estimate(e: &Entry, ram: &HashMap<String, u32>, cpu: &HashMap<u32, f64>) -> (Option<u32>, Option<f64>) {
    let r = e.app_ids.iter().find_map(|a| ram.get(a).copied());
    let w = e.app_ids.iter().find_map(|a| cpu.get(&fnv1a(a)).copied()).map(watts_from_util);
    (r, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_named_wins() {
        // the case seen in niri: "foot" put "Yazi (foot)" (a file manager run in foot) above Foot
        let yazi = score("foot", "Yazi (foot) shell;prompt; Terminal file manager").unwrap() * 4 + name_bonus("foot", "Yazi (foot)");
        let foot = score("foot", "Foot foot;terminal; Terminal").unwrap() * 4 + name_bonus("foot", "Foot");
        assert!(foot > yazi + 6, "Foot {foot} vs Yazi {yazi}");
        assert_eq!(name_bonus("fire", "Firefox"), 200);
        assert_eq!(name_bonus("code", "Visual Studio Code"), 100);
        assert_eq!(name_bonus("", "Foot"), 0);
    }

    #[test]
    fn fuzzy_and_markov() {
        assert!(score("ff", "Firefox").unwrap() > score("ff", "Office Suite").unwrap_or(-99));
        assert!(score("xyz", "Firefox").is_none());
        let mut m = HashMap::new();
        m.insert(("shadow-term".to_string(), "app:firefox.desktop".to_string()), 3);
        m.insert(("shadow-term".to_string(), "app:code.desktop".to_string()), 1);
        assert_eq!(markov_score(&m, "shadow-term", "app:firefox.desktop"), 75);
        assert_eq!(markov_score(&m, "other", "app:firefox.desktop"), 0);
        assert!((watts_from_util(0.5) - 9.5).abs() < 1e-9);
    }
}
