//! The Craft App Suite (ArtCraft): what exists, what is installed, and installing / updating /
//! removing an app from its GitHub releases, per user and with no administrator rights.
//!
//! The catalog and the asset naming follow the official ArtCraft Launcher
//! (github.com/storytold/craft-launcher, MIT / Apache-2.0).
//!
//! Safety rules:
//! - **Release data:** comes only from the GitHub releases API of the app's own repository.
//! - **Asset name:** an asset is used only when its name is exactly
//!   `<id>-<version>-linux-<arch>.<AppImage|tar.gz>` for the release being installed.
//! - **Download address:** it must point at that repository's `releases/download/` and must not
//!   contain "..".
//! - **Checksum:** every file is checked against its SHA-256 before anything is installed. The
//!   checksum comes from GitHub's asset digest, or the release's SHA256SUMS.txt. If there is
//!   neither, the app is not installed.
//! - **Location:** apps go to ~/Applications/Craft/<id>/, with a desktop entry in
//!   ~/.local/share/applications/craft-<id>.desktop. Shadow's launcher and hub list them like any
//!   other app.
use crate::{atomic_write, cmd_out, home};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct App {
    pub id: &'static str,
    pub name: &'static str,
    pub tagline: &'static str,
    pub color: (u8, u8, u8),
    pub repo: &'static str,
    /// desktop-entry categories
    pub cats: &'static str,
}

const fn app(id: &'static str, name: &'static str, tagline: &'static str, color: (u8, u8, u8), repo: &'static str, cats: &'static str) -> App {
    App { id, name, tagline, color, repo, cats }
}

/// The suite in the official launcher's order, then the community app.
pub const SUITE: [App; 16] = [
    app("photocraft", "PhotoCraft", "Image editing", (0x2f, 0x7b, 0xf5), "storytold/photocraft", "Graphics;2DGraphics;RasterGraphics;"),
    app("vectorcraft", "VectorCraft", "Vector illustration", (0xe8, 0x57, 0x3f), "storytold/vectorcraft", "Graphics;2DGraphics;VectorGraphics;"),
    app("filmcraft", "FilmCraft", "Video editing, color and sound", (0x8b, 0x5c, 0xf6), "storytold/filmcraft", "AudioVideo;Video;AudioVideoEditing;"),
    app("lightcraft", "LightCraft", "Photo library and raw development", (0xf2, 0xa5, 0x16), "storytold/lightcraft", "Graphics;Photography;"),
    app("pdfcraft", "PdfCraft", "Reading, organizing and protecting PDFs", (0xe8, 0x20, 0x4f), "storytold/pdfcraft", "Office;Viewer;"),
    app("effectcraft", "EffectCraft", "Motion graphics and visual effects", (0xe0, 0x36, 0x8f), "storytold/effectcraft", "AudioVideo;Video;"),
    app("designcraft", "DesignCraft", "Page layout and publishing", (0x7b, 0xb5, 0x1c), "storytold/designcraft", "Office;Publishing;"),
    app("figcraft", "FigCraft", "Interface design and prototyping", (0x02, 0xda, 0x96), "storytold/figcraft", "Graphics;Development;"),
    app("wordcraft", "WordCraft", "Writing and document design", (0x3f, 0x5f, 0xe0), "storytold/wordcraft", "Office;WordProcessor;"),
    app("soundcraft", "SoundCraft", "Recording, editing and mixing audio", (0x1f, 0xa5, 0xc9), "storytold/soundcraft", "AudioVideo;Audio;Recorder;"),
    app("deckcraft", "DeckCraft", "Presentations and slide shows", (0xf0, 0x7a, 0x1e), "storytold/deckcraft", "Office;Presentation;"),
    app("cadcraft", "CADCraft", "Computer-aided design and drafting", (0x3e, 0xa6, 0xe8), "storytold/cadcraft", "Graphics;Engineering;"),
    app("gridcraft", "GridCraft", "Spreadsheets and data", (0x1f, 0x9d, 0x55), "storytold/gridcraft", "Office;Spreadsheet;"),
    app("texturecraft", "TextureCraft", "3D texture painting", (0xc2, 0x67, 0x2b), "storytold/texturecraft", "Graphics;3DGraphics;"),
    app("craft-anything", "Craft Anything", "Find out how any software works, then craft your own", (0x4e, 0x7b, 0xfb), "storytold/craft-anything", "Development;"),
    app("solidcraft", "SolidCraft", "Parametric 3D CAD (community app by Tangency Labs)", (0x46, 0xa1, 0xfb), "tangency-labs/solidcraft", "Graphics;Engineering;3DGraphics;"),
];

pub fn by_id(id: &str) -> Option<&'static App> {
    SUITE.iter().find(|a| a.id == id)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    AppImage,
    TarGz,
}

impl Kind {
    fn ext(&self) -> &'static str {
        match self {
            Kind::AppImage => "AppImage",
            Kind::TarGz => "tar.gz",
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            Kind::AppImage => "AppImage",
            Kind::TarGz => "tar.gz",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub assets: Vec<Asset>,
}

pub fn parse_release(json: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("release data: {e}"))?;
    if let Some(m) = v.get("message").and_then(|m| m.as_str()) {
        return Err(format!("GitHub: {m}"));
    }
    let tag = v.get("tag_name").and_then(|t| t.as_str()).ok_or("release data: no tag")?;
    let version = tag.trim_start_matches('v').to_string();
    if version.is_empty() || !version.chars().all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c)) {
        return Err(format!("release data: odd version {tag:?}"));
    }
    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Asset {
                        name: x.get("name")?.as_str()?.to_string(),
                        url: x.get("browser_download_url")?.as_str()?.to_string(),
                        size: x.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                        sha256: x.get("digest").and_then(|d| d.as_str()).and_then(|d| d.strip_prefix("sha256:")).filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).map(|h| h.to_lowercase()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release { version, assets })
}

pub fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

fn url_ok(app: &App, url: &str) -> bool {
    url.starts_with(&format!("https://github.com/{}/releases/download/", app.repo)) && !url.contains("..") && !url.contains('?') && !url.contains('#')
}

/// The asset to install for this machine: AppImage first, then the tarball. Exact names only.
pub fn pick<'a>(app: &App, rel: &'a Release, arch: &str) -> Option<(&'a Asset, Kind)> {
    for kind in [Kind::AppImage, Kind::TarGz] {
        let want = format!("{}-{}-linux-{arch}.{}", app.id, rel.version, kind.ext());
        if let Some(a) = rel.assets.iter().find(|a| a.name == want && url_ok(app, &a.url)) {
            return Some((a, kind));
        }
    }
    None
}

/// What the release offers for Linux when there is nothing Shadow can install per user
pub fn other_linux_packages(app: &App, rel: &Release) -> Vec<String> {
    rel.assets.iter().filter(|a| a.name.starts_with(&format!("{}-", app.id)) && a.name.contains("-linux-")).map(|a| a.name.clone()).collect()
}

/// SHA-256 for `name` from a SHA256SUMS.txt body
pub fn sum_from_list(list: &str, name: &str) -> Option<String> {
    list.lines().find_map(|l| {
        let mut w = l.split_whitespace();
        let (h, n) = (w.next()?, w.next()?);
        (n.trim_start_matches('*') == name && h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| h.to_lowercase())
    })
}

// ── installed apps ───────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct Installed {
    pub version: String,
    pub kind: Kind,
    /// the program to run
    pub exe: PathBuf,
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"))
}

pub fn apps_dir() -> PathBuf {
    std::env::var_os("SHADOW_CRAFT_DIR").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join("Applications/Craft"))
}

fn manifest_path() -> PathBuf {
    data_home().join("shadow-craft/installed")
}

fn desktop_path(id: &str) -> PathBuf {
    data_home().join(format!("applications/craft-{id}.desktop"))
}

/// id → installed version (lines "id version kind exe")
pub fn installed() -> BTreeMap<String, Installed> {
    let mut m = BTreeMap::new();
    for l in std::fs::read_to_string(manifest_path()).unwrap_or_default().lines() {
        let mut w = l.splitn(4, ' ');
        if let (Some(id), Some(v), Some(k), Some(exe)) = (w.next(), w.next(), w.next(), w.next()) {
            let kind = if k == "tar.gz" { Kind::TarGz } else { Kind::AppImage };
            let exe = PathBuf::from(exe);
            if exe.exists() {
                m.insert(id.to_string(), Installed { version: v.to_string(), kind, exe });
            }
        }
    }
    m
}

fn save_installed(m: &BTreeMap<String, Installed>) -> Result<(), String> {
    let t: String = m.iter().map(|(id, i)| format!("{id} {} {} {}\n", i.version, i.kind.name(), i.exe.display())).collect();
    atomic_write(&manifest_path(), &t).map_err(|e| e.to_string())
}

/// AppImages mount themselves with FUSE 2; without it they can still run by unpacking first
fn have_fuse2() -> bool {
    ["/usr/lib64/libfuse.so.2", "/usr/lib/libfuse.so.2", "/usr/lib/x86_64-linux-gnu/libfuse.so.2", "/usr/lib/aarch64-linux-gnu/libfuse.so.2"].iter().any(|p| Path::new(p).exists())
}

pub fn launch_cmd(i: &Installed) -> String {
    let exe = crate::sh_quote(&i.exe.to_string_lossy());
    if i.kind == Kind::AppImage && !have_fuse2() {
        format!("env APPIMAGE_EXTRACT_AND_RUN=1 {exe}")
    } else {
        exe
    }
}

pub fn desktop_entry(app: &App, i: &Installed) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={}\nGenericName={}\nComment={} (Craft App Suite, installed by Shadow)\nExec={}\nIcon=applications-graphics\nTerminal=false\nCategories={}\nKeywords=craft;artcraft;{};\nX-Shadow-Craft={}\n",
        app.name,
        app.tagline,
        app.tagline,
        launch_cmd(i),
        app.cats,
        app.id,
        app.id
    )
}

// ── network and files ────────────────────────────────────────────────────────────────────────────

/// The newest release of an app, from the GitHub API (curl; no account needed)
pub fn fetch_release(app: &App) -> Result<Release, String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", app.repo);
    let (c, o) = cmd_out(&["curl", "-fsSL", "-m", "30", "-H", "Accept: application/vnd.github+json", &url]);
    if c == 127 {
        return Err("curl is not installed".into());
    }
    if c != 0 {
        return Err(format!("could not reach GitHub ({})", o.lines().last().unwrap_or("").trim()));
    }
    parse_release(&o)
}

pub fn sha256_file(p: &Path) -> Result<String, String> {
    let (c, o) = cmd_out(&["sha256sum", &p.to_string_lossy()]);
    if c != 0 {
        return Err(format!("sha256sum: {}", o.trim()));
    }
    o.split_whitespace().next().map(|h| h.to_lowercase()).ok_or_else(|| "sha256sum: no output".into())
}

/// Install (or update to) the newest release. `fetch` downloads a URL to a path (curl in real
/// use; tests give their own). Returns the new installation.
pub fn install_with(app: &App, rel: &Release, fetch: &dyn Fn(&str, &Path) -> Result<(), String>) -> Result<Installed, String> {
    let Some((asset, kind)) = pick(app, rel, arch()) else {
        let other = other_linux_packages(app, rel);
        return Err(if other.is_empty() {
            format!("{} {} has no Linux build for {} yet", app.name, rel.version, arch())
        } else {
            format!("{} {} for Linux comes only as {} — open its releases page to install it", app.name, rel.version, other.join(", "))
        });
    };
    // the expected checksum: GitHub's digest, else SHA256SUMS.txt from the same release
    let dir = apps_dir().join(app.id);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let want = match &asset.sha256 {
        Some(h) => h.clone(),
        None => {
            let sums = rel.assets.iter().find(|a| a.name == "SHA256SUMS.txt" && url_ok(app, &a.url)).ok_or_else(|| format!("{}: no checksum published for {} — not installing it", app.name, asset.name))?;
            let tmp = dir.join(".SHA256SUMS.txt.part");
            fetch(&sums.url, &tmp)?;
            let list = std::fs::read_to_string(&tmp).unwrap_or_default();
            let _ = std::fs::remove_file(&tmp);
            sum_from_list(&list, &asset.name).ok_or_else(|| format!("{}: {} is not in SHA256SUMS.txt — not installing it", app.name, asset.name))?
        }
    };
    let part = dir.join(format!(".{}.part", asset.name));
    let _ = std::fs::remove_file(&part);
    fetch(&asset.url, &part)?;
    let got = sha256_file(&part)?;
    if got != want {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{}: checksum mismatch for {} — the download was discarded", app.name, asset.name));
    }
    let exe = match kind {
        Kind::AppImage => {
            let dest = dir.join(&asset.name);
            std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            dest
        }
        Kind::TarGz => {
            let out = dir.join(format!("{}-{}", app.id, rel.version));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            // no absolute paths or ".." in the archive
            let (c, list) = cmd_out(&["tar", "-tzf", &part.to_string_lossy()]);
            if c != 0 || list.lines().any(|l| l.starts_with('/') || l.split('/').any(|p| p == "..")) {
                let _ = std::fs::remove_file(&part);
                return Err(format!("{}: the archive is unreadable or unsafe — not installed", app.name));
            }
            let (c, o) = cmd_out(&["tar", "-xzf", &part.to_string_lossy(), "-C", &out.to_string_lossy(), "--no-same-owner"]);
            let _ = std::fs::remove_file(&part);
            if c != 0 {
                return Err(format!("{}: unpacking failed: {}", app.name, o.trim()));
            }
            find_program(&out, app.id).ok_or_else(|| format!("{}: no `{}` program inside the archive", app.name, app.id))?
        }
    };
    let new = Installed { version: rel.version.clone(), kind, exe };
    // replace the previous version only now that the new one is in place
    let mut m = installed();
    if let Some(old) = m.get(app.id) {
        if old.exe != new.exe {
            remove_files(old);
        }
    }
    m.insert(app.id.to_string(), new.clone());
    save_installed(&m)?;
    atomic_write(&desktop_path(app.id), &desktop_entry(app, &new)).map_err(|e| e.to_string())?;
    let _ = cmd_out(&["update-desktop-database", &data_home().join("applications").to_string_lossy()]);
    Ok(new)
}

pub fn install(app: &App, rel: &Release) -> Result<Installed, String> {
    install_with(app, rel, &|url, to| {
        let (c, o) = cmd_out(&["curl", "-fL", "--proto", "=https", "--retry", "2", "-m", "1800", "-o", &to.to_string_lossy(), url]);
        if c == 0 {
            Ok(())
        } else {
            Err(format!("download failed: {}", o.lines().last().unwrap_or("").trim()))
        }
    })
}

fn find_program(dir: &Path, id: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).ok()?.flatten() {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).ok()?;
            if m.is_dir() {
                stack.push(p);
            } else if m.is_file() && p.file_name().is_some_and(|n| n == id) && m.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

fn remove_files(i: &Installed) {
    match i.kind {
        Kind::AppImage => {
            let _ = std::fs::remove_file(&i.exe);
        }
        Kind::TarGz => {
            // the unpacked "<id>-<version>" folder directly under the app's folder
            let root = apps_dir();
            let mut p = i.exe.clone();
            while let Some(parent) = p.parent() {
                if parent.parent() == Some(root.as_path()) || parent == root {
                    break;
                }
                p = parent.to_path_buf();
            }
            if p.starts_with(&root) && p != root {
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }
}

pub fn remove(app: &App) -> Result<(), String> {
    let mut m = installed();
    let Some(i) = m.remove(app.id) else { return Err(format!("{} is not installed", app.name)) };
    remove_files(&i);
    let _ = std::fs::remove_file(desktop_path(app.id));
    let _ = std::fs::remove_dir(apps_dir().join(app.id));
    save_installed(&m)
}

/// "0.10.0" > "0.9.2": numeric parts compared in order
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| s.split(['.', '-', '+']).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parse(a) > parse(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    const REL: &str = r#"{"tag_name":"v0.9.0","assets":[
      {"name":"photocraft-0.9.0-macos-universal.dmg","size":9,"browser_download_url":"https://github.com/storytold/photocraft/releases/download/v0.9.0/photocraft-0.9.0-macos-universal.dmg","digest":"sha256:06503eb1c845990e99f8824831cc73b274e2b1dbe4461eb6460dfc9a6008239d"},
      {"name":"photocraft-0.9.0-linux-x86_64.tar.gz","size":9,"browser_download_url":"https://github.com/storytold/photocraft/releases/download/v0.9.0/photocraft-0.9.0-linux-x86_64.tar.gz"},
      {"name":"photocraft-0.9.0-linux-x86_64.AppImage","size":9,"browser_download_url":"https://github.com/storytold/photocraft/releases/download/v0.9.0/photocraft-0.9.0-linux-x86_64.AppImage","digest":"sha256:DIGEST"},
      {"name":"SHA256SUMS.txt","size":9,"browser_download_url":"https://github.com/storytold/photocraft/releases/download/v0.9.0/SHA256SUMS.txt"}]}"#;

    #[test]
    fn picks_the_appimage_by_exact_name_from_the_right_repo() {
        let r = parse_release(&REL.replace("DIGEST", &"a".repeat(64))).unwrap();
        let app = by_id("photocraft").unwrap();
        let (a, k) = pick(app, &r, "x86_64").unwrap();
        assert_eq!((a.name.as_str(), k), ("photocraft-0.9.0-linux-x86_64.AppImage", Kind::AppImage));
        assert_eq!(a.sha256.as_deref(), Some(&*"a".repeat(64)));
        // an asset whose address leaves the repository is never used
        let evil = REL.replace("storytold/photocraft/releases/download/v0.9.0/photocraft-0.9.0-linux-x86_64.AppImage", "attacker/repo/releases/download/v1/photocraft-0.9.0-linux-x86_64.AppImage").replace("DIGEST", &"a".repeat(64));
        let r = parse_release(&evil).unwrap();
        assert_eq!(pick(app, &r, "x86_64").unwrap().1, Kind::TarGz, "falls back to the genuine tarball");
        assert!(pick(app, &r, "aarch64").is_none());
        assert!(parse_release(r#"{"message":"API rate limit exceeded"}"#).unwrap_err().contains("rate limit"));
    }

    #[test]
    fn install_verifies_then_update_replaces_then_remove_cleans_up() {
        let d = std::env::temp_dir().join(format!("craft-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::env::set_var("SHADOW_CRAFT_DIR", d.join("apps"));
        std::env::set_var("XDG_DATA_HOME", d.join("data"));
        std::env::set_var("HOME", &d);
        let app = by_id("photocraft").unwrap();
        let payload = b"#!/bin/sh\necho photocraft\n".to_vec();
        let good = {
            let p = d.join("payload");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(&p, &payload).unwrap();
            sha256_file(&p).unwrap()
        };
        let fetch = |_: &str, to: &Path| std::fs::write(to, b"#!/bin/sh\necho photocraft\n").map_err(|e| e.to_string());
        // a wrong checksum: refused, nothing installed
        let bad = parse_release(&REL.replace("DIGEST", &"0".repeat(64))).unwrap();
        assert!(install_with(app, &bad, &fetch).unwrap_err().contains("checksum mismatch"));
        assert!(installed().is_empty());
        // the right one: installed, executable, listed, with a desktop entry
        let r = parse_release(&REL.replace("DIGEST", &good)).unwrap();
        let i = install_with(app, &r, &fetch).unwrap();
        assert_eq!(i.version, "0.9.0");
        assert!(i.exe.ends_with("photocraft/photocraft-0.9.0-linux-x86_64.AppImage"));
        assert_eq!(installed().get("photocraft"), Some(&i));
        let de = std::fs::read_to_string(desktop_path("photocraft")).unwrap();
        assert!(de.contains("Name=PhotoCraft") && de.contains("X-Shadow-Craft=photocraft"));
        // an update replaces the old file
        let r2 = parse_release(&REL.replace("0.9.0", "0.10.0").replace("DIGEST", &good)).unwrap();
        let i2 = install_with(app, &r2, &fetch).unwrap();
        assert!(!i.exe.exists() && i2.exe.exists() && newer("0.10.0", "0.9.0"));
        // remove: file, entry and listing gone
        remove(app).unwrap();
        assert!(!i2.exe.exists() && !desktop_path("photocraft").exists() && installed().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn checksum_list() {
        let l = format!("{}  photocraft-0.9.0-linux-x86_64.AppImage\n{} *other\n", "b".repeat(64), "c".repeat(64));
        assert_eq!(sum_from_list(&l, "photocraft-0.9.0-linux-x86_64.AppImage"), Some("b".repeat(64)));
        assert_eq!(sum_from_list(&l, "missing"), None);
    }
}
