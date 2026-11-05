//! FIELD TRAINING — the compositor's own hotkey tutorial.
//!
//! A small glass panel at the bottom of the screen asks for one hotkey at a time ("open the
//! menu: SUPER + SPACE"). You press the real key, the real thing happens, the step locks in with a
//! cyan flash and the next one slides up. It reads the keys from the live bind table, so a key you
//! rebound is the key it asks for, and a step whose bind you removed is left out.
//!
//! First login on this compositor opens it by itself (once: a marker in ~/.config/shadow-state);
//! Mod+Shift+/ or Mod+F1 opens it again. The panel's own buttons work with the mouse: SKIP and
//! CLOSE. While it runs, the lock step is only acknowledged — training never locks you out.
use std::time::{Duration, Instant};

/// (default key, what it does, extra line: the mouse way or a tip)
pub const STEPS: &[(&str, &str, &str)] = &[
    ("Mod+Space", "OPEN THE HIDDEN HUB", "the glyph star in the middle — or click the centre mark"),
    ("Mod+D", "LAUNCH ANYTHING", "apps, files, actions: + for Craft apps, > for actions"),
    ("Mod+Return", "OPEN A TERMINAL", "your default terminal (settings → DEFAULTS)"),
    ("Mod+Tab", "MOVE BETWEEN APPS", "or click an app; Mod+Shift+Return gives it the main tile"),
    ("Mod+M", "MINIMISE TO THE GRID", "it becomes a square tile at the bottom of the app area"),
    ("Mod+Shift+M", "BRING IT BACK", "or click its tile; scroll the grid with the wheel"),
    ("Mod+Q", "CLOSE THE FOCUSED APP", "the others re-tile"),
    ("Mod+Comma", "OPEN SETTINGS", "a web app: settings, fix center, config — Mod+Shift+H fixes"),
    ("Mod+Alt+L", "LOCK THE SCREEN", "(training only acknowledges it — nothing locks now)"),
    ("Mod+Shift+Slash", "OPEN THIS GUIDE AGAIN", "any time — Mod+F1 works too"),
];

const FLASH: Duration = Duration::from_millis(650);

pub struct Tutorial {
    /// the steps whose bind exists: (key shown, action it runs, title, tip)
    pub steps: Vec<(String, String, &'static str, &'static str)>,
    pub at: usize,
    /// the moment the current step was completed (the flash); None while waiting
    pub hit: Option<Instant>,
    pub opened: Instant,
    pub done: bool,
    /// when the last step was done (the panel says so for a moment, then closes)
    pub done_at: Option<Instant>,
}

impl Tutorial {
    /// `binds`: key → action, as compiled from the config
    pub fn new(binds: &std::collections::BTreeMap<String, String>) -> Tutorial {
        let steps = STEPS.iter().filter_map(|(key, title, tip)| {
            let action = binds.get(*key)?.clone();
            // the key the user really has for that action (a rebind keeps the step)
            Some((key.to_string(), action, *title, *tip))
        }).collect();
        let mut t = Tutorial { steps, at: 0, hit: None, opened: Instant::now(), done: false, done_at: None };
        if t.steps.is_empty() {
            t.done = true;
            t.done_at = Some(Instant::now());
        }
        t
    }

    /// An action ran (from a key, the menu or shadowctl). Returns true when it completed the step.
    pub fn observe(&mut self, action: &str) -> bool {
        if self.done || self.hit.is_some() {
            return false;
        }
        match self.steps.get(self.at) {
            Some((_, a, _, _)) if a == action => {
                self.hit = Some(Instant::now());
                true
            }
            _ => false,
        }
    }

    /// The lock step is acknowledged instead of run while training.
    pub fn swallows(&self, action: &str) -> bool {
        !self.done && action == "lock" && self.steps.get(self.at).map(|s| s.1 == "lock").unwrap_or(false)
    }

    pub fn skip(&mut self) {
        self.hit = None;
        self.at += 1;
        if self.at >= self.steps.len() {
            self.at = self.steps.len().saturating_sub(1);
            if !self.done {
                self.done = true;
                self.done_at = Some(Instant::now());
            }
        }
    }

    /// the "complete" message has been shown long enough
    pub fn expired(&self) -> bool {
        self.done_at.map(|t| t.elapsed() > Duration::from_secs(4)).unwrap_or(false)
    }

    /// advance after the flash; true while something is still moving
    pub fn tick(&mut self) -> bool {
        if self.done {
            return false;
        }
        if let Some(t) = self.hit {
            if t.elapsed() >= FLASH {
                self.skip();
            }
            return true;
        }
        self.opened.elapsed() < Duration::from_millis(400)
    }

    /// 0..1: how far the success flash is
    pub fn flash(&self) -> f32 {
        self.hit.map(|t| (t.elapsed().as_secs_f32() / FLASH.as_secs_f32()).min(1.0)).unwrap_or(0.0)
    }
    /// 0..1: the panel sliding up when opened
    pub fn rise(&self) -> f32 {
        let t = (self.opened.elapsed().as_secs_f32() / 0.4).min(1.0);
        1.0 - (1.0 - t).powi(3)
    }

    /// what the panel says: (text, kind) — 0 dim, 1 amber, 2 cyan, 3 bright
    pub fn lines(&self, modkey: &str) -> Vec<(String, u8)> {
        let n = self.steps.len();
        if self.done || n == 0 {
            return vec![
                ("FIELD TRAINING COMPLETE".into(), 2),
                ("everything above works with the mouse too — and Mod+Shift+/ brings this back".into(), 0),
            ];
        }
        let (key, _, title, tip) = &self.steps[self.at];
        let pips: String = (0..n).map(|i| if i < self.at { '■' } else if i == self.at { if self.hit.is_some() { '■' } else { '▣' } } else { '□' }).collect();
        vec![
            (format!("FIELD TRAINING  {pips}  {:02}/{:02}", self.at + 1, n), 1),
            (if self.hit.is_some() { format!("✓ {title}") } else { (*title).to_string() }, if self.hit.is_some() { 2 } else { 3 }),
            (pretty_key(key, modkey), 1),
            ((*tip).to_string(), 0),
        ]
    }
}

/// "Mod+Shift+BracketRight" → "SUPER + SHIFT + ]"
pub fn pretty_key(key: &str, modkey: &str) -> String {
    key.split('+').map(|p| match p.to_ascii_lowercase().as_str() {
        "mod" => if modkey == "alt" { "ALT".to_string() } else { "SUPER".to_string() },
        "super" | "logo" | "win" => "SUPER".into(),
        "bracketright" => "]".into(),
        "bracketleft" => "[".into(),
        "return" => "ENTER".into(),
        "comma" => ",".into(),
        "period" => ".".into(),
        "slash" => "/".into(),
        "backspace" => "BACKSPACE".into(),
        "escape" => "ESC".into(),
        "space" => "SPACE".into(),
        o => o.to_uppercase(),
    }).collect::<Vec<_>>().join(" + ")
}

fn marker() -> std::path::PathBuf {
    crate::config::state_dir().join("compositor-tutorial.done")
}

/// first login on this compositor: not yet trained
pub fn first_run() -> bool {
    std::env::var_os("SHADOW_NO_TUTORIAL").is_none() && !marker().exists()
}

pub fn mark_done() {
    let m = marker();
    if let Some(d) = m.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(m, "trained\n");
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn steps_follow_the_live_binds() {
        let mut b = crate::config::default_binds();
        b.insert("Mod+Return".into(), "spawn foot".into());
        b.remove("Mod+Q");
        let mut t = Tutorial::new(&b);
        assert_eq!(t.steps.len(), STEPS.len() - 1, "a removed bind drops its step");
        assert!(!t.observe("spawn shadow-launch"), "out of order does nothing");
        assert!(t.observe("spawn shadow-glyph summon"));
        assert!(!t.observe("spawn shadow-glyph summon"), "no double count during the flash");
        t.hit = Some(Instant::now() - FLASH);
        assert!(t.tick());
        assert_eq!(t.at, 1);
        while t.steps[t.at].1 != "spawn foot" {
            t.skip();
        }
        assert!(t.observe("spawn foot"), "the user's own terminal completes the terminal step");
        while t.steps[t.at].1 != "lock" {
            t.skip();
        }
        assert!(t.swallows("lock"));
        for _ in 0..20 {
            t.skip();
        }
        assert!(t.done);
        assert_eq!(t.lines("super")[0].0, "FIELD TRAINING COMPLETE");
    }

    #[test]
    fn keys_read_like_keycaps() {
        assert_eq!(pretty_key("Mod+BracketRight", "super"), "SUPER + ]");
        assert_eq!(pretty_key("Mod+Shift+Slash", "alt"), "ALT + SHIFT + /");
        assert_eq!(pretty_key("Mod+Return", "super"), "SUPER + ENTER");
        assert_eq!(pretty_key("Mod+Alt+L", "super"), "SUPER + ALT + L");
    }
}
