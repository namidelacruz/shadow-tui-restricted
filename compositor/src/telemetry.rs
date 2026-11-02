//! 32-byte telemetry record sent to shadow-powerd on every focus change / launch:
//!   0..8   u64 timestamp (ms since the Unix epoch)
//!   8..12  u32 app_class_id (FNV-1a of the app id)
//!  12..16  f32 key_cadence_hz (key presses per second over the last 5 s)
//!  16      u8  ac_state (1 = on AC)
//!  17      u8  battery_pct (255 = no battery)
//!  18..20  i16 cpu_temp_c
//!  20..32  reserved (zero)
//! Little-endian, sent as one datagram; dropped silently when the daemon isn't listening.
use crate::state::State;

pub fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

pub fn encode(ts_ms: u64, app: &str, cadence: f32, ac: bool, bat: Option<u8>, temp: Option<i32>) -> [u8; 32] {
    let mut b = [0u8; 32];
    b[0..8].copy_from_slice(&ts_ms.to_le_bytes());
    b[8..12].copy_from_slice(&fnv1a(app).to_le_bytes());
    b[12..16].copy_from_slice(&cadence.to_le_bytes());
    b[16] = ac as u8;
    b[17] = bat.unwrap_or(255);
    b[18..20].copy_from_slice(&(temp.unwrap_or(0).clamp(-128, 255) as i16).to_le_bytes());
    b
}

pub fn send(state: &State, app: &str) {
    let path = std::env::var("SHADOW_TELEMETRY_SOCK").unwrap_or_else(|_| "/run/shadow-powerd/telemetry.sock".into());
    let pw = crate::hud::power();
    let cadence = state.key_times.iter().filter(|t| t.elapsed().as_secs() < 5).count() as f32 / 5.0;
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let rec = encode(ts, app, cadence, pw.ac, pw.battery, crate::hud::package_temp());
    if let Ok(s) = std::os::unix::net::UnixDatagram::unbound() {
        let _ = s.set_nonblocking(true);
        let _ = s.send_to(&rec, path);
    }
}

#[cfg(test)]
mod t {
    #[test]
    fn layout() {
        let b = super::encode(1_700_000_000_123, "shadow-term", 3.5, true, Some(81), Some(52));
        assert_eq!(b.len(), 32);
        assert_eq!(u64::from_le_bytes(b[0..8].try_into().unwrap()), 1_700_000_000_123);
        assert_eq!(u32::from_le_bytes(b[8..12].try_into().unwrap()), super::fnv1a("shadow-term"));
        assert_eq!(f32::from_le_bytes(b[12..16].try_into().unwrap()), 3.5);
        assert_eq!((b[16], b[17]), (1, 81));
        assert_eq!(i16::from_le_bytes(b[18..20].try_into().unwrap()), 52);
        assert!(b[20..].iter().all(|x| *x == 0));
    }
}
