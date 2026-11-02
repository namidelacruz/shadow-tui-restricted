//! Wallpapers: decoding (PNG / JPEG), scaling to a monitor (fill · fit · center · tile), the
//! generated default, and the [wallpaper] configuration — shared by shadow-wallpaper and
//! shadow-hub. Special paths: "vertices" (the 3D wireframe, see `vertex`) and "default" (the
//! generated aurora image).
use crate::cfg::{get_in, set_in};
use crate::{atomic_write, config_path};
use std::path::Path;

/// RGB image, 8 bits per channel
#[derive(Clone)]
pub struct Img {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
}

const MAX_PIXELS: usize = 80_000_000;



pub fn decode(path: &Path) -> Result<Img, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if data.starts_with(b"\x89PNG") {
        let mut dec = png::Decoder::new(std::io::Cursor::new(&data));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut r = dec.read_info().map_err(|e| e.to_string())?;
        let (w, h) = (r.info().width as usize, r.info().height as usize);
        if w * h > MAX_PIXELS {
            return Err("image too large".into());
        }
        let mut buf = vec![0; r.output_buffer_size()];
        let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
        let ch = info.color_type.samples();
        let mut rgb = Vec::with_capacity(w * h * 3);
        for px in buf[..info.buffer_size()].chunks_exact(ch) {
            match ch {
                1 | 2 => rgb.extend_from_slice(&[px[0], px[0], px[0]]),
                _ => {
                    // alpha over the obsidian background
                    let a = if ch == 4 { px[3] as u32 } else { 255 };
                    for (k, c) in px[..3].iter().enumerate() {
                        let bg = [10u32, 11, 14][k];
                        rgb.push(((*c as u32 * a + bg * (255 - a)) / 255) as u8);
                    }
                }
            }
        }
        return Ok(Img { w, h, rgb });
    }
    if data.starts_with(&[0xFF, 0xD8]) {
        let opts = zune_jpeg::zune_core::options::DecoderOptions::default().jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB);
        let mut dec = zune_jpeg::JpegDecoder::new_with_options(&data, opts);
        dec.decode_headers().map_err(|e| format!("{e:?}"))?;
        let (w, h) = dec.dimensions().ok_or("no dimensions")?;
        if w * h > MAX_PIXELS {
            return Err("image too large".into());
        }
        let rgb = dec.decode().map_err(|e| format!("{e:?}"))?;
        return Ok(Img { w, h, rgb });
    }
    Err(format!("{}: not a PNG or JPEG image", path.display()))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mode {
    Fill,
    Fit,
    Center,
    Tile,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "fit" => Mode::Fit,
            "center" => Mode::Center,
            "tile" => Mode::Tile,
            _ => Mode::Fill,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Mode::Fill => "fill",
            Mode::Fit => "fit",
            Mode::Center => "center",
            Mode::Tile => "tile",
        }
    }
}

/// Scale into a w×h BGRA (opaque) buffer. Bilinear when scaling.
pub fn compose(img: &Img, w: usize, h: usize, mode: Mode) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    for p in out.chunks_exact_mut(4) {
        p.copy_from_slice(&[14, 11, 10, 255]);
    }
    let sample = |x: f32, y: f32| -> [u8; 3] {
        let x = x.clamp(0.0, (img.w - 1) as f32);
        let y = y.clamp(0.0, (img.h - 1) as f32);
        let (x0, y0) = (x as usize, y as usize);
        let (x1, y1) = ((x0 + 1).min(img.w - 1), (y0 + 1).min(img.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let px = |xx: usize, yy: usize, c: usize| img.rgb[(yy * img.w + xx) * 3 + c] as f32;
        let mut o = [0u8; 3];
        for (c, v) in o.iter_mut().enumerate() {
            let top = px(x0, y0, c) * (1.0 - fx) + px(x1, y0, c) * fx;
            let bot = px(x0, y1, c) * (1.0 - fx) + px(x1, y1, c) * fx;
            *v = (top * (1.0 - fy) + bot * fy).round() as u8;
        }
        o
    };
    let put = |out: &mut [u8], x: usize, y: usize, c: [u8; 3]| {
        let i = (y * w + x) * 4;
        out[i] = c[2];
        out[i + 1] = c[1];
        out[i + 2] = c[0];
        out[i + 3] = 255;
    };
    match mode {
        Mode::Fill | Mode::Fit => {
            let s = if mode == Mode::Fill { (w as f32 / img.w as f32).max(h as f32 / img.h as f32) } else { (w as f32 / img.w as f32).min(h as f32 / img.h as f32) };
            let (dw, dh) = (img.w as f32 * s, img.h as f32 * s);
            let (ox, oy) = ((w as f32 - dw) / 2.0, (h as f32 - dh) / 2.0);
            for y in 0..h {
                let sy = (y as f32 + 0.5 - oy) / s - 0.5;
                if sy < -0.5 || sy > img.h as f32 - 0.5 {
                    continue;
                }
                for x in 0..w {
                    let sx = (x as f32 + 0.5 - ox) / s - 0.5;
                    if sx < -0.5 || sx > img.w as f32 - 0.5 {
                        continue;
                    }
                    put(&mut out, x, y, sample(sx, sy));
                }
            }
        }
        Mode::Center | Mode::Tile => {
            let (ox, oy) = ((w as i64 - img.w as i64) / 2, (h as i64 - img.h as i64) / 2);
            for y in 0..h {
                for x in 0..w {
                    let (sx, sy) = if mode == Mode::Tile {
                        ((x % img.w) as i64, (y % img.h) as i64)
                    } else {
                        (x as i64 - ox, y as i64 - oy)
                    };
                    if sx < 0 || sy < 0 || sx >= img.w as i64 || sy >= img.h as i64 {
                        continue;
                    }
                    let i = (sy as usize * img.w + sx as usize) * 3;
                    put(&mut out, x, y, [img.rgb[i], img.rgb[i + 1], img.rgb[i + 2]]);
                }
            }
        }
    }
    out
}

/// The generated Shadow wallpaper: a deep obsidian gradient, soft aurora ribbons in amber,
/// violet and cyan, a faint HUD horizon grid and scan lines. BGRA, opaque.
pub fn default_wallpaper(w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    let (fw, fh) = (w as f32, h as f32);
    let ribbons: [(f32, f32, f32, f32, [f32; 3]); 3] = [
        // (base y, amplitude, frequency, thickness, colour)
        (0.42, 0.10, 1.7, 0.055, [255.0, 176.0, 0.0]),
        (0.50, 0.13, 1.2, 0.080, [155.0, 107.0, 255.0]),
        (0.58, 0.08, 2.3, 0.040, [0.0, 229.0, 255.0]),
    ];
    let horizon = 0.72;
    for y in 0..h {
        let v = y as f32 / fh;
        // obsidian: a little lighter and bluer at the top
        let base = [10.0 + 8.0 * (1.0 - v), 11.0 + 10.0 * (1.0 - v), 16.0 + 18.0 * (1.0 - v)];
        let scan = if y % 4 == 0 { 0.92 } else { 1.0 };
        for x in 0..w {
            let u = x as f32 / fw;
            let mut c = base;
            for (i, (by, amp, freq, th, col)) in ribbons.iter().enumerate() {
                let cy = by + amp * (std::f32::consts::TAU * (u * freq + i as f32 * 0.21)).sin() * (0.6 + 0.4 * (u * 3.1).cos());
                let d = (v - cy) / th;
                // soft core + wide glow, fading towards the edges of the screen
                let k = (-d * d).exp() * 0.28 + (-d * d * 0.08).exp() * 0.07;
                let edge = (u * 4.0).min((1.0 - u) * 4.0).min(1.0);
                for j in 0..3 {
                    c[j] += col[j] * k * edge;
                }
            }
            // horizon grid below the ribbons, in perspective
            if v > horizon {
                let depth = (v - horizon) / (1.0 - horizon);
                let gy = (1.0 / (depth + 0.02)).fract();
                let gx = ((u - 0.5) / (depth + 0.05) * 6.0).fract().abs();
                let line = ((gy < 0.06) as u8 as f32 + (gx < 0.03 || gx > 0.97) as u8 as f32).min(1.0);
                let fade = depth * 0.10;
                for j in 0..3 {
                    c[j] += [0.0, 229.0, 255.0][j] * line * fade;
                }
            }
            // vignette
            let vg = 1.0 - 0.35 * ((u - 0.5).powi(2) + (v - 0.5).powi(2)) * 2.0;
            let i = (y * w + x) * 4;
            out[i] = (c[2] * vg * scan).clamp(0.0, 255.0) as u8;
            out[i + 1] = (c[1] * vg * scan).clamp(0.0, 255.0) as u8;
            out[i + 2] = (c[0] * vg * scan).clamp(0.0, 255.0) as u8;
            out[i + 3] = 255;
        }
    }
    out
}

// ── configuration ───────────────────────────────────────────────────────────────────────────────

pub fn conf() -> String {
    std::fs::read_to_string(config_path()).unwrap_or_default()
}

/// (path or "default", mode) for an output
pub fn choice(text: &str, output: Option<&str>) -> (String, Mode) {
    let path = output
        .and_then(|o| get_in(text, "wallpaper.outputs", o))
        .or_else(|| get_in(text, "wallpaper", "path"))
        .map(|p| p.trim_matches('"').to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "vertices".into());
    let mode = get_in(text, "wallpaper", "mode").map(|m| Mode::parse(m.trim_matches('"'))).unwrap_or(Mode::Fill);
    (path, mode)
}

pub fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn set(path: &str, output: Option<&str>, mode: Option<Mode>) -> Result<(), String> {
    if path != "default" && path != "vertices" {
        decode(Path::new(path))?; // never point the config at something that won't load
    }
    let mut t = conf();
    t = match output {
        Some(o) => set_in(&t, "wallpaper.outputs", &q(o), &q(path)),
        None => set_in(&t, "wallpaper", "path", &q(path)),
    };
    if let Some(m) = mode {
        t = set_in(&t, "wallpaper", "mode", &q(m.name()));
    }
    atomic_write(&config_path(), &t).map_err(|e| e.to_string())?;
    // the running wallpaper reloads on SIGUSR1. Process names are cut at 15 characters, so match
    // the command line: the daemon runs without arguments (the picker has "pick")
    let _ = std::process::Command::new("pkill").args(["-USR1", "-f", "^(/[^ ]*/)?shadow-wallpaper$"]).status();
    // the hub draws the wallpaper itself when it is the desktop
    let _ = std::process::Command::new("pkill").args(["-USR1", "-f", "^(/[^ ]*/)?shadow-hub$"]).status();
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_place_the_image() {
        // a 2×1 image: red | blue
        let img = Img { w: 2, h: 1, rgb: vec![255, 0, 0, 0, 0, 255] };
        let fill = compose(&img, 4, 4, Mode::Fill);
        assert_eq!(fill[3], 255, "opaque");
        let fit = compose(&img, 4, 4, Mode::Fit);
        assert_eq!(&fit[0..3], &[14, 11, 10], "letterbox stays background");
        let tile = compose(&img, 4, 1, Mode::Tile);
        assert_eq!((tile[2], tile[8 + 2]), (255, 255), "tiles repeat (R in BGRA)");
        let center = compose(&img, 4, 1, Mode::Center);
        assert_eq!(center[4 + 2], 255, "centred at x=1");
    }

    #[test]
    fn default_is_dark_and_opaque_with_colour() {
        let p = default_wallpaper(160, 100);
        assert!(p.chunks_exact(4).all(|x| x[3] == 255));
        let lum: u64 = p.chunks_exact(4).map(|x| x[0] as u64 + x[1] as u64 + x[2] as u64).sum();
        assert!(lum / (160 * 100 * 3) < 80, "a dark wallpaper");
        assert!(p.chunks_exact(4).any(|x| x[2] > 60), "with warm ribbons");
    }

    #[test]
    fn png_round_trip() {
        let d = std::env::temp_dir().join(format!("wp-{}.png", std::process::id()));
        {
            let f = std::fs::File::create(&d).unwrap();
            let mut e = png::Encoder::new(f, 2, 2);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]).unwrap();
        }
        let img = decode(&d).unwrap();
        assert_eq!((img.w, img.h, &img.rgb[0..3]), (2, 2, &[255u8, 0, 0][..]));
        let _ = std::fs::remove_file(&d);
    }
}
