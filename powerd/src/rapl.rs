//! Package power clamping through intel-rapl (powercap): constraint_0 = PL1 (long term),
//! constraint_1 = PL2 (short term), in microwatts. The firmware values are saved the first time
//! and are the ceiling — Shadow lowers limits to save power and restores them for heavy work, but
//! never raises the package above what the OEM firmware configured.
use crate::sys;

const BASE: &str = "sys/class/powercap/intel-rapl:0";

pub fn available() -> bool {
    sys::exists(&format!("{BASE}/constraint_0_power_limit_uw"))
}

fn rd(n: u8) -> Option<u64> {
    sys::read(&format!("{BASE}/constraint_{n}_power_limit_uw")).and_then(|v| v.parse().ok())
}

fn saved_path() -> std::path::PathBuf {
    sys::p("var/lib/shadow-powerd/rapl-firmware")
}

/// firmware (PL1, PL2) in µW, recorded once
pub fn firmware() -> Option<(u64, u64)> {
    if let Some(s) = sys::read_path(&saved_path()) {
        let v: Vec<u64> = s.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if v.len() == 2 {
            return Some((v[0], v[1]));
        }
    }
    let (a, b) = (rd(0)?, rd(1).unwrap_or(0));
    let _ = std::fs::create_dir_all(saved_path().parent().unwrap());
    let _ = std::fs::write(saved_path(), format!("{a} {b}\n"));
    Some((a, b))
}

/// current PL1 in watts
pub fn pl1_w() -> Option<f64> {
    rd(0).map(|u| u as f64 / 1e6)
}

/// Set PL1 to `watts` (None = firmware). PL2 follows: firmware, or 1.5×PL1 when clamped.
pub fn set(watts: Option<f64>) -> Result<String, String> {
    if !available() {
        return Err("intel-rapl not available".into());
    }
    let (fw1, fw2) = firmware().ok_or("cannot read the firmware limits")?;
    let pl1 = watts.map(|w| ((w * 1e6) as u64).min(fw1)).unwrap_or(fw1);
    let pl2 = if watts.is_some() && fw2 > 0 { ((pl1 as f64 * 1.5) as u64).min(fw2) } else { fw2 };
    sys::write(&format!("{BASE}/constraint_0_power_limit_uw"), &pl1.to_string())?;
    if fw2 > 0 {
        let _ = sys::write(&format!("{BASE}/constraint_1_power_limit_uw"), &pl2.to_string());
    }
    Ok(format!("PL1 {:.1} W · PL2 {:.1} W", pl1 as f64 / 1e6, pl2 as f64 / 1e6))
}

/// PL1 for a mode (W); None = firmware
pub fn for_mode(mode: &str) -> Option<f64> {
    match mode {
        "low-power" | "cool" => Some(9.0),
        "quiet" => Some(12.0),
        _ => None,
    }
}

#[cfg(test)]
mod t {
    #[test]
    fn clamp_never_exceeds_firmware() {
        let r = std::env::temp_dir().join(format!("rapl-{}", std::process::id()));
        let b = r.join("sys/class/powercap/intel-rapl:0");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("constraint_0_power_limit_uw"), "17000000").unwrap();
        std::fs::write(b.join("constraint_1_power_limit_uw"), "30000000").unwrap();
        let _env = crate::sys::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("SHADOW_OPT_ROOT", &r);
        assert_eq!(super::firmware(), Some((17_000_000, 30_000_000)));
        assert_eq!(super::set(Some(9.0)).unwrap(), "PL1 9.0 W · PL2 13.5 W");
        assert_eq!(super::set(Some(50.0)).unwrap(), "PL1 17.0 W · PL2 25.5 W");
        assert_eq!(super::set(None).unwrap(), "PL1 17.0 W · PL2 30.0 W");
        std::env::remove_var("SHADOW_OPT_ROOT");
    }
}
