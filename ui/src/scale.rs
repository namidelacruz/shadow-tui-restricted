//! Self-resolving scale.
//!
//! [`ideal_scale`] turns a monitor's pixel size and physical size (from its EDID) into the scale
//! that gives comfortable, consistent text: laptop panels are viewed closer than desk monitors,
//! so they get a higher reference density. shadow-displayd applies this to niri for every
//! monitor as it is connected. Each app also checks the scale it is actually given against the
//! same ideal; if they disagree by a quarter step or more (the compositor kept scale 1 on a
//! 2560x1600 panel, a user override, an EDID quirk), [`text_correction`] scales the text so it
//! still comes out at the intended physical size.

/// Built-in panel connectors (viewed at ~35-45 cm).
pub fn is_internal(connector: &str) -> bool {
    // test hook for nested sessions: SHADOW_FAKE_INTERNAL=winit
    if std::env::var("SHADOW_FAKE_INTERNAL").is_ok_and(|v| v == connector) {
        return true;
    }
    let c = connector.to_ascii_uppercase();
    c.starts_with("EDP") || c.starts_with("LVDS") || c.starts_with("DSI")
}

/// Ideal scale in quarter steps, 1.0..=3.0. `None` when the monitor doesn't report a believable
/// physical size (projectors, virtual outputs, aspect-ratio placeholder EDIDs).
pub fn ideal_scale(connector: &str, px: (i32, i32), mm: (i32, i32)) -> Option<f64> {
    if px.0 <= 0 || mm.0 < 100 || mm.1 < 60 {
        return None;
    }
    // reject EDIDs that only encode the aspect ratio (16x9 cm style placeholders)
    if (mm.0 == 160 && mm.1 == 90) || (mm.0 == 160 && mm.1 == 100) {
        return None;
    }
    let dpi = px.0 as f64 / (mm.0 as f64 / 25.4);
    let reference = if is_internal(connector) { 144.0 } else { 110.0 };
    Some(((dpi / reference * 4.0).round() / 4.0).clamp(1.0, 3.0))
}

/// Factor applied to text sizes when the compositor's scale is not the ideal one.
pub fn text_correction(actual: f64, ideal: Option<f64>) -> f64 {
    match ideal {
        Some(i) if actual > 0.0 && (i - actual).abs() >= 0.25 - 1e-9 => (i / actual).clamp(0.5, 3.0),
        _ => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dell_14_plus_panel_is_1_5() {
        // 2560x1600 14" 16:10 ~ 301x188 mm
        assert_eq!(ideal_scale("eDP-1", (2560, 1600), (301, 188)), Some(1.5));
    }

    #[test]
    fn desk_monitors() {
        assert_eq!(ideal_scale("DP-1", (3840, 2160), (597, 336)), Some(1.5)); // 27" 4K
        assert_eq!(ideal_scale("DP-2", (2560, 1440), (597, 336)), Some(1.0)); // 27" QHD
        assert_eq!(ideal_scale("HDMI-A-1", (1920, 1080), (527, 296)), Some(1.0)); // 24" FHD
        assert_eq!(ideal_scale("DP-3", (3840, 2160), (698, 393)), Some(1.25)); // 32" 4K
    }

    #[test]
    fn unknown_sizes() {
        assert_eq!(ideal_scale("HDMI-A-1", (1920, 1080), (0, 0)), None);
        assert_eq!(ideal_scale("HDMI-A-1", (1920, 1080), (160, 90)), None);
    }

    #[test]
    fn correction_only_when_off() {
        assert_eq!(text_correction(1.5, Some(1.5)), 1.0);
        assert_eq!(text_correction(1.0, Some(1.5)), 1.5);
        assert_eq!(text_correction(1.25, Some(1.5)), 1.2);
        assert_eq!(text_correction(1.0, None), 1.0);
    }
}
