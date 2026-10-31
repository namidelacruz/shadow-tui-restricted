//! shadowctl — talk to the running shadow-compositor.
//!   shadowctl state | action focus-next | spawn CMD | focus ID | lock | refresh 60 | config-reload | lua-reload
use std::io::{BufRead, BufReader, Write};

fn socket() -> Option<std::path::PathBuf> {
    if let Some(s) = std::env::var_os("SHADOW_SOCKET") {
        return Some(s.into());
    }
    let dir = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").ok()?);
    let mut v: Vec<_> = std::fs::read_dir(&dir).ok()?.flatten().map(|e| e.path())
        .filter(|p| p.file_name().map_or(false, |n| n.to_string_lossy().starts_with("shadow-compositor.") && n.to_string_lossy().ends_with(".sock"))).collect();
    v.sort();
    v.pop()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        println!("usage: shadowctl state | action ACTION | spawn CMD | focus ID | lock | refresh HZ | config-reload | lua-reload");
        println!("actions: focus-next focus-prev width-up width-down fullscreen close hud-toggle healer-revert lock quit");
        return;
    }
    let Some(p) = socket() else {
        eprintln!("shadowctl: shadow-compositor is not running (no control socket)");
        std::process::exit(2);
    };
    let mut s = match std::os::unix::net::UnixStream::connect(&p) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("shadowctl: {}: {e}", p.display());
            std::process::exit(2);
        }
    };
    let _ = s.write_all(format!("{}\n", args.join(" ")).as_bytes());
    let mut r = String::new();
    let _ = BufReader::new(&s).read_line(&mut r);
    print!("{r}");
    if r.starts_with("error") {
        std::process::exit(1);
    }
}
