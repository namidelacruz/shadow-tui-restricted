//! Predictive power loop (spec §3.2). The compositor sends a 32-byte record on every focus change
//! ([timestamp, app_class_id, key_cadence_hz, ac_state, battery_pct, cpu_temp_c]). For each app
//! class the daemon learns how much CPU the following 30 s actually used (exponentially weighted),
//! and before the next session with that class it feeds forward:
//!   low-compute (reading / typing) on battery → PL1 9 W, EPP power, ACPI profile cool
//!   compute-heavy (compiles, media)           → back to firmware PL1/PL2 and a higher profile
//!                                               *before* the load arrives and throttles.
//! The model is a per-class statistical table: it runs in microseconds on the CPU. The NPU is used
//! only when an ONNX model and runtime are installed (see `shadow-powerd npu`); waking the NPU for
//! a lookup this small would cost more energy than it saves.
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Record {
    pub ts_ms: u64,
    pub class: u32,
    pub cadence: f32,
    pub ac: bool,
    pub battery: Option<u8>,
    pub temp_c: i16,
}

pub fn decode(b: &[u8]) -> Option<Record> {
    if b.len() != 32 {
        return None;
    }
    Some(Record {
        ts_ms: u64::from_le_bytes(b[0..8].try_into().ok()?),
        class: u32::from_le_bytes(b[8..12].try_into().ok()?),
        cadence: f32::from_le_bytes(b[12..16].try_into().ok()?),
        ac: b[16] == 1,
        battery: (b[17] != 255).then_some(b[17]),
        temp_c: i16::from_le_bytes(b[18..20].try_into().ok()?),
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Load {
    Low,
    High,
    Unknown,
}

#[derive(Default)]
pub struct Model {
    /// class → (ewma CPU utilisation 0..1, samples)
    pub table: HashMap<u32, (f64, u32)>,
    active: Option<(u32, std::time::Instant, (u64, u64))>,
    /// set by the learning schedule: after the learning phase, known classes adapt slowly
    pub stable: bool,
}

pub const MIN_SAMPLES: u32 = 3;
pub const LOW: f64 = 0.12;
pub const HIGH: f64 = 0.50;

impl Model {
    pub fn predict(&self, class: u32) -> Load {
        match self.table.get(&class) {
            Some((u, n)) if *n >= MIN_SAMPLES && *u < LOW => Load::Low,
            Some((u, n)) if *n >= MIN_SAMPLES && *u > HIGH => Load::High,
            _ => Load::Unknown,
        }
    }

    /// learn from an observed utilisation for a class; returns the value predicted before it
    pub fn observe(&mut self, class: u32, util: f64) -> Option<f64> {
        let stable = self.stable;
        let e = self.table.entry(class).or_insert((util, 0));
        let prior = (e.1 >= MIN_SAMPLES).then_some(e.0);
        let alpha = crate::schedule::alpha(e.1, stable);
        e.0 = if e.1 == 0 { util } else { (1.0 - alpha) * e.0 + alpha * util };
        e.1 = (e.1 + 1).min(10_000);
        prior
    }

    /// a focus record arrived: close the previous window of observation and open a new one
    pub fn focus(&mut self, class: u32, cpu: (u64, u64)) -> Option<(u32, f64, Option<f64>)> {
        let closed = self.close(cpu);
        self.active = Some((class, std::time::Instant::now(), cpu));
        closed
    }

    /// every poll: a class that has been focused for 30 s gets a sample
    pub fn tick(&mut self, cpu: (u64, u64)) -> Option<(u32, f64, Option<f64>)> {
        match self.active {
            Some((_, t, _)) if t.elapsed().as_secs() >= 30 => {
                let r = self.close(cpu);
                if let Some((c, _, _)) = r {
                    self.active = Some((c, std::time::Instant::now(), cpu));
                }
                r
            }
            _ => None,
        }
    }

    fn close(&mut self, cpu: (u64, u64)) -> Option<(u32, f64, Option<f64>)> {
        let (class, t, (busy0, total0)) = self.active.take()?;
        if t.elapsed().as_secs() < 5 || cpu.1 <= total0 {
            return None;
        }
        let util = (cpu.0.saturating_sub(busy0)) as f64 / (cpu.1 - total0) as f64;
        let prior = self.observe(class, util);
        Some((class, util, prior))
    }

    pub fn save(&self) -> String {
        let mut v: Vec<_> = self.table.iter().collect();
        v.sort_by_key(|(k, _)| **k);
        v.into_iter().map(|(k, (u, n))| format!("{k} {u:.4} {n}\n")).collect()
    }
    pub fn load(text: &str) -> Model {
        let mut m = Model::default();
        for l in text.lines() {
            let f: Vec<&str> = l.split_whitespace().collect();
            if let (Some(k), Some(u), Some(n)) = (f.first().and_then(|x| x.parse().ok()), f.get(1).and_then(|x| x.parse().ok()), f.get(2).and_then(|x| x.parse().ok())) {
                m.table.insert(k, (u, n));
            }
        }
        m
    }
}

/// (busy, total) jiffies from /proc/stat
pub fn cpu_times() -> (u64, u64) {
    let s = crate::sys::read("proc/stat").unwrap_or_default();
    let v: Vec<u64> = s.lines().next().unwrap_or("").split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
    let total: u64 = v.iter().sum();
    let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
    (total.saturating_sub(idle), total)
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn decode_and_learn() {
        let mut b = [0u8; 32];
        b[0..8].copy_from_slice(&123u64.to_le_bytes());
        b[8..12].copy_from_slice(&77u32.to_le_bytes());
        b[12..16].copy_from_slice(&2.5f32.to_le_bytes());
        b[16] = 0;
        b[17] = 64;
        b[18..20].copy_from_slice(&51i16.to_le_bytes());
        let r = decode(&b).unwrap();
        assert_eq!((r.class, r.ac, r.battery, r.temp_c), (77, false, Some(64), 51));
        assert!(decode(&b[..31]).is_none());
        let mut m = Model::default();
        assert_eq!(m.predict(1), Load::Unknown);
        for _ in 0..3 {
            m.observe(1, 0.04); // an editor: light
            m.observe(2, 0.85); // a compiler: heavy
        }
        assert_eq!((m.predict(1), m.predict(2)), (Load::Low, Load::High));
        let m2 = Model::load(&m.save());
        assert_eq!(m2.predict(2), Load::High);
    }
}
