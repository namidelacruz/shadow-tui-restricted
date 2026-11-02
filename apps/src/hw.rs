//! What machine this is, read once from /proc and /sys: CPU, memory, GPU driver, battery,
//! virtual machine or WSL. Shadow uses it to pick sensible defaults on any computer rather than
//! assuming one laptop: the hub's field quality, whether glass and blur are worth their cost,
//! and the session's backend under WSL.
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// 2 threads or fewer, under 4 GB, or no GPU acceleration (software rendering)
    Low,
    Mid,
    /// 8+ threads and 16 GB+ with a real GPU
    High,
}

#[derive(Clone, Debug)]
pub struct Profile {
    pub cpu: String,
    pub threads: usize,
    pub ram_gb: f32,
    /// kernel drivers of the GPUs: i915, xe, amdgpu, nouveau, nvidia, virtio_gpu, …
    pub gpus: Vec<String>,
    pub battery: bool,
    pub wsl: bool,
    pub vm: bool,
    pub tier: Tier,
}

fn root() -> PathBuf {
    std::env::var_os("SHADOW_SYS_ROOT").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn rd(r: &Path, rel: &str) -> String {
    std::fs::read_to_string(r.join(rel)).unwrap_or_default()
}

/// Read the profile from a root (tests point this at a fake tree).
pub fn read(r: &Path) -> Profile {
    let cpuinfo = rd(r, "proc/cpuinfo");
    let cpu = cpuinfo.lines().find_map(|l| l.strip_prefix("model name").and_then(|v| v.split_once(':')).map(|(_, v)| v.trim().to_string())).unwrap_or_else(|| "unknown CPU".into());
    let threads = cpuinfo.lines().filter(|l| l.starts_with("processor")).count().max(1);
    let ram_kb: f32 = rd(r, "proc/meminfo").lines().find_map(|l| l.strip_prefix("MemTotal:")).and_then(|v| v.split_whitespace().next()?.parse().ok()).unwrap_or(0.0);
    let mut gpus = vec![];
    if let Ok(rdir) = std::fs::read_dir(r.join("sys/class/drm")) {
        let mut names: Vec<_> = rdir.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.starts_with("card") && !n.contains('-')).collect();
        names.sort();
        for n in names {
            let link = r.join("sys/class/drm").join(&n).join("device/driver");
            if let Ok(t) = std::fs::read_link(&link) {
                if let Some(d) = t.file_name() {
                    let d = d.to_string_lossy().to_string();
                    if !gpus.contains(&d) {
                        gpus.push(d);
                    }
                }
            }
        }
    }
    let osrel = rd(r, "proc/sys/kernel/osrelease").to_lowercase();
    let wsl = osrel.contains("microsoft") || osrel.contains("wsl") || r.join("proc/sys/fs/binfmt_misc/WSLInterop").exists() || std::env::var_os("WSL_DISTRO_NAME").is_some_and(|v| !v.is_empty());
    let vendor = rd(r, "sys/class/dmi/id/sys_vendor").to_lowercase();
    let product = rd(r, "sys/class/dmi/id/product_name").to_lowercase();
    let vm = wsl || cpuinfo.contains(" hypervisor") || ["qemu", "kvm", "vmware", "virtualbox", "innotek", "microsoft corporation", "xen", "bochs", "parallels"].iter().any(|v| vendor.contains(v) || product.contains(v));
    let battery = std::fs::read_dir(r.join("sys/class/power_supply")).map(|d| d.flatten().any(|e| rd(r, &format!("sys/class/power_supply/{}/type", e.file_name().to_string_lossy())).trim() == "Battery")).unwrap_or(false);
    let ram_gb = ram_kb / 1048576.0;
    // a software-only GPU (none, or a virtual one without 3D) means the compositor renders on the CPU
    let soft_gpu = gpus.is_empty() || gpus.iter().all(|g| matches!(g.as_str(), "vmwgfx" | "bochs-drm" | "bochs" | "cirrus" | "simpledrm" | "efifb" | "hyperv_drm" | "vboxvideo" | "qxl"));
    // WSLg composites on the Windows side; the GPU shows up as /dev/dxg rather than a DRM card
    let soft_gpu = soft_gpu && !(wsl && r.join("dev/dxg").exists());
    let tier = if threads <= 2 || ram_gb < 3.5 || soft_gpu {
        Tier::Low
    } else if threads >= 8 && ram_gb >= 15.0 {
        Tier::High
    } else {
        Tier::Mid
    };
    Profile { cpu, threads, ram_gb, gpus, battery, wsl, vm, tier }
}

/// This machine (read once)
pub fn profile() -> Profile {
    static P: OnceLock<Profile> = OnceLock::new();
    P.get_or_init(|| read(&root())).clone()
}

impl Profile {
    pub fn summary(&self) -> String {
        format!(
            "{} · {} threads · {:.0} GB · GPU {} · {}{}{} · tier {:?}",
            self.cpu,
            self.threads,
            self.ram_gb,
            if self.gpus.is_empty() { "none".into() } else { self.gpus.join("+") },
            if self.battery { "battery" } else { "mains only" },
            if self.wsl { " · WSL" } else { "" },
            if self.vm && !self.wsl { " · virtual machine" } else { "" },
            self.tier
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(cpus: usize, mem_kb: u64, driver: Option<&str>, osrel: &str, battery: bool) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hw-{}-{cpus}-{mem_kb}-{}", std::process::id(), driver.unwrap_or("none")));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("proc/sys/kernel")).unwrap();
        let ci: String = (0..cpus).map(|i| format!("processor\t: {i}\nmodel name\t: Test CPU\n\n")).collect();
        std::fs::write(d.join("proc/cpuinfo"), ci).unwrap();
        std::fs::write(d.join("proc/meminfo"), format!("MemTotal:       {mem_kb} kB\n")).unwrap();
        std::fs::write(d.join("proc/sys/kernel/osrelease"), osrel).unwrap();
        if let Some(drv) = driver {
            let dev = d.join("sys/class/drm/card0/device");
            std::fs::create_dir_all(&dev).unwrap();
            let target = d.join(format!("sys/bus/pci/drivers/{drv}"));
            std::fs::create_dir_all(&target).unwrap();
            std::os::unix::fs::symlink(&target, dev.join("driver")).unwrap();
        }
        if battery {
            std::fs::create_dir_all(d.join("sys/class/power_supply/BAT0")).unwrap();
            std::fs::write(d.join("sys/class/power_supply/BAT0/type"), "Battery\n").unwrap();
        }
        d
    }

    #[test]
    fn tiers_from_hardware() {
        // a Lunar Lake laptop: 8 threads, 32 GB, xe, battery
        let p = read(&tree(8, 32_000_000, Some("xe"), "7.1.3-200.fc44.x86_64", true));
        assert_eq!((p.tier, p.gpus.clone(), p.battery, p.wsl), (Tier::High, vec!["xe".to_string()], true, false));
        // an old dual-core netbook
        assert_eq!(read(&tree(2, 4_000_000, Some("i915"), "6.8", true)).tier, Tier::Low);
        // a desktop with no battery
        let d = read(&tree(4, 8_000_000, Some("amdgpu"), "6.8", false));
        assert_eq!((d.tier, d.battery), (Tier::Mid, false));
        // WSL2 without a GPU device: software rendering
        let w = read(&tree(8, 16_000_000, None, "6.6.36.6-microsoft-standard-WSL2", false));
        assert!(w.wsl && w.vm);
        assert_eq!(w.tier, Tier::Low);
    }
}
