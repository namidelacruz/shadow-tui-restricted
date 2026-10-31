//! File operations for shadow-files (no shell involved).
use crate::home;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub dir: bool,
    pub link: bool,
    pub size: u64,
    pub mtime: u64,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Sort {
    Name,
    Modified,
    Size,
}

pub fn list(dir: &Path, hidden: bool, sort: Sort) -> Vec<Entry> {
    let mut v: Vec<Entry> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    if !hidden && name.starts_with('.') {
                        return None;
                    }
                    let path = e.path();
                    let lmeta = std::fs::symlink_metadata(&path).ok()?;
                    let meta = std::fs::metadata(&path).unwrap_or(lmeta.clone());
                    Some(Entry {
                        name,
                        dir: meta.is_dir(),
                        link: lmeta.file_type().is_symlink(),
                        size: meta.len(),
                        mtime: meta.modified().ok().and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs()),
                        path,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| {
        b.dir.cmp(&a.dir).then_with(|| match sort {
            Sort::Name => natural(&a.name, &b.name),
            Sort::Modified => b.mtime.cmp(&a.mtime),
            Sort::Size => b.size.cmp(&a.size),
        })
    });
    v
}

/// Case-insensitive natural order: "file2" before "file10".
pub fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n1 = String::new();
                while let Some(c) = x.peek().copied().filter(|c| c.is_ascii_digit()) {
                    n1.push(c);
                    x.next();
                }
                let mut n2 = String::new();
                while let Some(c) = y.peek().copied().filter(|c| c.is_ascii_digit()) {
                    n2.push(c);
                    y.next();
                }
                let o = n1.trim_start_matches('0').len().cmp(&n2.trim_start_matches('0').len())
                    .then_with(|| n1.trim_start_matches('0').cmp(n2.trim_start_matches('0')));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

pub fn human(n: u64) -> String {
    let u = ["B", "K", "M", "G", "T"];
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < u.len() - 1 {
        f /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n}B") } else { format!("{f:.1}{}", u[i]) }
}

pub fn is_text(p: &Path) -> bool {
    use std::io::Read;
    let mut buf = [0u8; 4096];
    let Ok(mut f) = std::fs::File::open(p) else { return false };
    let n = f.read(&mut buf).unwrap_or(0);
    !buf[..n].contains(&0) && std::str::from_utf8(&buf[..n]).is_ok() || n == 0
}

pub fn png_size(p: &Path) -> Option<(u32, u32)> {
    let b = std::fs::read(p).ok()?;
    if b.len() > 24 && &b[..8] == b"\x89PNG\r\n\x1a\n" {
        let w = u32::from_be_bytes(b[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(b[20..24].try_into().ok()?);
        return Some((w, h));
    }
    None
}

pub fn preview(e: &Entry, hidden: bool, max: usize) -> Vec<String> {
    if e.dir {
        let l = list(&e.path, hidden, Sort::Name);
        if l.is_empty() {
            return vec!["(empty)".into()];
        }
        return l.iter().take(max).map(|x| if x.dir { format!("{}/", x.name) } else { x.name.clone() }).collect();
    }
    if e.size <= 2 * 1024 * 1024 && is_text(&e.path) {
        let text = std::fs::read_to_string(&e.path).unwrap_or_default();
        return text.lines().take(max).map(|l| l.replace('\t', "    ").chars().filter(|c| !c.is_control()).collect()).collect();
    }
    let mut v = vec![format!("{}  ·  {}", human(e.size), ext_kind(&e.name))];
    if let Some((w, h)) = png_size(&e.path) {
        v.push(format!("PNG image {w}×{h}"));
    }
    v.push(String::new());
    v.push("o: open with the default app".into());
    v
}

fn ext_kind(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "avif" => "image",
        "mp4" | "mkv" | "webm" | "mov" => "video",
        "mp3" | "flac" | "ogg" | "wav" | "opus" => "audio",
        "pdf" => "PDF document",
        "zip" | "tar" | "gz" | "xz" | "zst" | "7z" => "archive",
        "docx" | "odt" | "doc" => "document",
        "xlsx" | "ods" | "csv" => "spreadsheet",
        _ => "file",
    }
}

// ---------------------------------------------------------------- file operations
pub fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(src)?;
        return std::os::unix::fs::symlink(target, dst);
    }
    if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_recursive(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dst).map(|_| ())
    }
}

/// "name", "name (2)", "name (3)"… so pasting never overwrites.
pub fn free_name(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if std::fs::symlink_metadata(&p).is_err() {
        return p;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| std::fs::symlink_metadata(p).is_err()).unwrap()
}

pub fn move_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        // across filesystems: copy, then remove
        Err(e) if e.raw_os_error() == Some(18) => {
            copy_recursive(src, dst)?;
            if src.is_dir() { std::fs::remove_dir_all(src) } else { std::fs::remove_file(src) }
        }
        Err(e) => Err(e),
    }
}

fn url_encode(p: &str) -> String {
    p.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// freedesktop.org Trash (~/.local/share/Trash), so file managers and `gio trash --restore` see it.
pub fn trash(p: &Path) -> std::io::Result<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share")).join("Trash");
    std::fs::create_dir_all(base.join("files"))?;
    std::fs::create_dir_all(base.join("info"))?;
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let dst = free_name(&base.join("files"), &name);
    let dname = dst.file_name().unwrap().to_string_lossy().into_owned();
    let abs = std::fs::canonicalize(p.parent().unwrap_or(Path::new("/"))).unwrap_or_default().join(p.file_name().unwrap_or_default());
    let date = crate::cmd_out(&["date", "+%Y-%m-%dT%H:%M:%S"]).1;
    std::fs::write(base.join("info").join(format!("{dname}.trashinfo")),
        format!("[Trash Info]\nPath={}\nDeletionDate={}\n", url_encode(&abs.to_string_lossy()), date.trim()))?;
    move_path(p, &dst)?;
    Ok(dst)
}

