//! Which package world we're in: Arch (pacman) or the Red Hat family (Fedora, RHEL, CentOS
//! Stream, Rocky, Alma, Oracle — dnf/rpm). Shared by `shadow`, `shadow-powerd` and `shadow-sec`
//! (std only), so every "install X" hint and every package operation uses the right tool and the
//! right package name.
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Family {
    Arch,
    Rpm,
    Other,
}

fn root() -> std::path::PathBuf {
    std::env::var_os("SHADOW_SYS_ROOT").or_else(|| std::env::var_os("SHADOW_OPT_ROOT")).map(std::path::PathBuf::from).unwrap_or_else(|| "/".into())
}

/// key from /etc/os-release (unquoted)
pub fn os_release(key: &str) -> Option<String> {
    let t = std::fs::read_to_string(root().join("etc/os-release")).ok()?;
    t.lines().find_map(|l| l.strip_prefix(&format!("{key}=")).map(|v| v.trim().trim_matches('"').to_string()))
}

pub fn family() -> Family {
    if let Ok(f) = std::env::var("SHADOW_DISTRO") {
        return match f.as_str() {
            "arch" => Family::Arch,
            "rpm" | "fedora" | "rhel" => Family::Rpm,
            _ => Family::Other,
        };
    }
    let id = format!("{} {}", os_release("ID").unwrap_or_default(), os_release("ID_LIKE").unwrap_or_default());
    if id.split_whitespace().any(|w| w == "arch") || Path::new("/usr/bin/pacman").exists() && !Path::new("/usr/bin/rpm").exists() {
        Family::Arch
    } else if id.split_whitespace().any(|w| matches!(w, "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "ol")) || Path::new("/usr/bin/dnf").exists() {
        Family::Rpm
    } else {
        Family::Other
    }
}

/// Arch package name -> the Fedora/EL name (None = not packaged there)
pub fn pkg(arch_name: &str) -> Option<&'static str> {
    let m: &[(&str, Option<&str>)] = &[
        ("greetd", Some("greetd")),
        ("age", Some("age")),
        ("github-cli", Some("gh")),
        ("iwd", Some("iwd")),
        ("wpa_supplicant", Some("wpa_supplicant")),
        ("networkmanager", Some("NetworkManager")),
        ("sof-firmware", Some("alsa-sof-firmware")),
        ("alsa-ucm-conf", Some("alsa-ucm")),
        ("fsverity-utils", Some("fsverity-utils")),
        ("nftables", Some("nftables")),
        ("audit", Some("audit")),
        ("openssh", Some("openssh")),
        ("cryptsetup", Some("cryptsetup")),
        ("terminus-font", Some("terminus-fonts-console")),
        ("webkit2gtk-4.1", Some("webkit2gtk4.1")),
        ("gtk3", Some("gtk3")),
        ("libsoup3", Some("libsoup3")),
        ("gtk-layer-shell", Some("gtk-layer-shell")),
        ("pipewire", Some("pipewire")),
        ("wireplumber", Some("wireplumber")),
        ("pipewire-pulse", Some("pipewire-pulseaudio")),
        ("brightnessctl", Some("brightnessctl")),
        ("linux-lts", None),
        // fix center: session pieces the compositor leaves to the distribution
        ("xwayland-satellite", Some("xwayland-satellite")),
        ("xdg-desktop-portal-gnome", Some("xdg-desktop-portal-gnome")),
        ("xdg-desktop-portal-gtk", Some("xdg-desktop-portal-gtk")),
        ("mako", Some("mako")),
        ("mate-polkit", Some("mate-polkit")),
        ("gnome-keyring", Some("gnome-keyring")),
        ("libsecret", Some("libsecret")),
    ];
    m.iter().find(|(a, _)| *a == arch_name).and_then(|(_, r)| *r)
}

/// "sudo pacman -S x" / "sudo dnf install x" — for messages
pub fn install_hint(arch_names: &[&str]) -> String {
    match family() {
        Family::Rpm => {
            let v: Vec<&str> = arch_names.iter().filter_map(|n| pkg(n)).collect();
            if v.is_empty() {
                "not packaged for this distribution".into()
            } else {
                format!("sudo dnf install {}", v.join(" "))
            }
        }
        _ => format!("sudo pacman -S {}", arch_names.join(" ")),
    }
}

fn quiet(cmd: &[&str]) -> bool {
    Command::new(cmd[0]).args(&cmd[1..]).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

/// Is the package (given by its Arch name) installed?
pub fn installed(arch_name: &str) -> bool {
    match family() {
        Family::Rpm => pkg(arch_name).is_some_and(|p| quiet(&["rpm", "-q", p])),
        _ => quiet(&["pacman", "-Q", arch_name]),
    }
}

/// Is the package (given by its Arch name) available from the configured repositories?
pub fn available(arch_name: &str) -> bool {
    match family() {
        Family::Rpm => pkg(arch_name).is_some_and(|p| quiet(&["dnf", "-q", "info", p])),
        _ => quiet(&["pacman", "-Si", arch_name]),
    }
}

/// Install packages (Arch names) non-interactively as root. Returns success.
pub fn install(arch_names: &[&str]) -> bool {
    match family() {
        Family::Rpm => {
            let v: Vec<&str> = arch_names.iter().filter_map(|n| pkg(n)).collect();
            if v.is_empty() {
                return false;
            }
            let mut c = vec!["dnf", "install", "-y"];
            c.extend(v);
            quiet(&c)
        }
        _ => {
            let mut c = vec!["pacman", "-S", "--needed", "--noconfirm"];
            c.extend_from_slice(arch_names);
            quiet(&c)
        }
    }
}

/// Packages the user installed explicitly (for state sync): (repo packages, foreign/AUR)
pub fn user_packages() -> Option<(String, String)> {
    let out = |c: &[&str]| Command::new(c[0]).args(&c[1..]).stderr(Stdio::null()).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    match family() {
        Family::Rpm => out(&["dnf", "-q", "repoquery", "--userinstalled", "--queryformat", "%{name}\n"]).map(|s| (s.lines().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n"), String::new())),
        _ => Some((out(&["pacman", "-Qqen"])?, out(&["pacman", "-Qqem"]).unwrap_or_default())),
    }
}

/// argv to install a list of repo package names (already in this distro's naming)
pub fn install_argv(yes: bool) -> Vec<&'static str> {
    match family() {
        Family::Rpm => {
            let mut v = vec!["sudo", "dnf", "install", "--skip-broken"];
            if yes {
                v.push("-y");
            }
            v
        }
        _ => {
            let mut v = vec!["sudo", "pacman", "-S", "--needed"];
            if yes {
                v.push("--noconfirm");
            }
            v
        }
    }
}

/// installed (name, version) of the first of `names` that is installed
pub fn installed_version(names: &[&str]) -> Option<(String, String)> {
    for n in names {
        let r = match family() {
            Family::Rpm => Command::new("rpm").args(["-q", "--queryformat", "%{VERSION}-%{RELEASE}", n]).output(),
            _ => Command::new("pacman").args(["-Q", n]).output(),
        };
        if let Ok(o) = r {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                let v = if family() == Family::Rpm { s } else { s.split_whitespace().nth(1).unwrap_or("").to_string() };
                return Some((n.to_string(), v));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_hints() {
        assert_eq!(pkg("github-cli"), Some("gh"));
        assert_eq!(pkg("sof-firmware"), Some("alsa-sof-firmware"));
        assert_eq!(pkg("linux-lts"), None);
        std::env::set_var("SHADOW_DISTRO", "rpm");
        assert_eq!(install_hint(&["age", "github-cli"]), "sudo dnf install age gh");
        assert_eq!(install_hint(&["linux-lts"]), "not packaged for this distribution");
        std::env::set_var("SHADOW_DISTRO", "arch");
        assert_eq!(install_hint(&["age"]), "sudo pacman -S age");
        std::env::remove_var("SHADOW_DISTRO");
    }
}
