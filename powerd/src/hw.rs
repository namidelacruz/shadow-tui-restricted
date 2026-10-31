//! Hardware detection: what this machine is and which controls the kernel exposes.
use crate::sys;

#[derive(Debug, Default, Clone)]
pub struct Codec {
    pub link: String,
    pub vendor: String,
    pub part: String,
}

#[derive(Debug, Default, Clone)]
pub struct Hw {
    pub vendor: String,
    pub product: String,
    pub sku: String,
    pub dell: bool,
    pub dell_14_plus: bool,
    pub cpu: String,
    pub lunar_lake: bool,
    // NPU
    pub npu_kernel: bool,     // /sys/class/accel/accel* bound to intel_vpu
    pub npu_node: bool,       // /dev/accel/accel0
    pub npu_firmware: bool,   // /usr/lib/firmware/intel/vpu/*
    pub npu_userspace: bool,  // Level Zero NPU driver library
    pub openvino: bool,
    // GPU
    pub gpu_driver: String,   // i915 / xe
    // audio
    pub audio_pci: Option<String>,
    pub audio_driver: String,
    pub codecs: Vec<Codec>,
    // power controls
    pub profile_choices: Vec<String>,
    pub profile_path: Option<String>,
    pub battery: Option<String>,       // BAT0
    pub charge_types: Vec<String>,     // tokens available
    pub charge_thresholds: bool,
    pub epp_choices: Vec<String>,
    pub ac_supply: Option<String>,
}

fn pci_devices() -> Vec<(String, String, String, Option<String>)> {
    // (address, vendor, class, driver)
    sys::ls("sys/bus/pci/devices")
        .into_iter()
        .map(|a| {
            let base = format!("sys/bus/pci/devices/{a}");
            (
                a.clone(),
                sys::read(&format!("{base}/vendor")).unwrap_or_default(),
                sys::read(&format!("{base}/class")).unwrap_or_default(),
                sys::link_name(&format!("{base}/driver")),
            )
        })
        .collect()
}

/// "RT722", "CS42L43" … from the SoundWire manufacturer + part ids
pub fn codec_name(c: &Codec) -> String {
    let part = c.part.trim_start_matches('0');
    match c.vendor.as_str() {
        "025d" => format!("Realtek RT{part}"),
        "01fa" => match c.part.as_str() {
            "4243" => "Cirrus CS42L43".into(),
            "3556" => "Cirrus CS35L56".into(),
            "3557" => "Cirrus CS35L57".into(),
            p => format!("Cirrus {p}"),
        },
        v => format!("{} {}", codec_vendor(v), c.part),
    }
}

pub fn codec_vendor(id: &str) -> &'static str {
    match id {
        "025d" => "Realtek",
        "01fa" => "Cirrus Logic",
        "0102" => "TI",
        "019f" => "Maxim",
        _ => "unknown",
    }
}

pub fn detect() -> Hw {
    let mut h = Hw::default();
    h.vendor = sys::read("sys/class/dmi/id/sys_vendor").unwrap_or_default();
    h.product = sys::read("sys/class/dmi/id/product_name").unwrap_or_default();
    h.sku = sys::read("sys/class/dmi/id/product_sku").unwrap_or_default();
    h.dell = h.vendor.to_lowercase().contains("dell");
    let prod = h.product.to_lowercase();
    h.dell_14_plus = h.dell && (prod.contains("14 plus") || prod.contains("db14250"));

    h.cpu = sys::read("proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_default();
    // Core Ultra 200V = Lunar Lake ("Intel(R) Core(TM) Ultra 7 258V"); the audio controller id is a second signal
    let lnl_cpu = {
        let c = h.cpu.replace("(TM)", "").replace("(R)", "");
        c.contains("Core Ultra") && c.split_whitespace().last().map_or(false, |m| m.len() == 4 && m.starts_with('2') && m.ends_with('V'))
    };

    for (addr, vendor, class, driver) in pci_devices() {
        let dev = sys::read(&format!("sys/bus/pci/devices/{addr}/device")).unwrap_or_default();
        if vendor == "0x8086" && (class.starts_with("0x0403") || class.starts_with("0x0401")) {
            h.audio_pci = Some(format!("{addr} [8086:{}]", dev.trim_start_matches("0x")));
            h.audio_driver = driver.clone().unwrap_or_default();
            if dev == "0xa828" {
                h.lunar_lake = true;
            }
        }
        if class.starts_with("0x0300") && vendor == "0x8086" {
            h.gpu_driver = driver.unwrap_or_default();
        }
    }
    h.lunar_lake |= lnl_cpu;

    for d in sys::ls("sys/bus/soundwire/devices") {
        // sdw:<link>:<unique>:<mfg>:<part>:<class>
        let f: Vec<&str> = d.split(':').collect();
        if f.len() >= 5 && f[0] == "sdw" {
            h.codecs.push(Codec { link: f[1].into(), vendor: f[3].into(), part: f[4].into() });
        }
    }

    // NPU
    for a in sys::ls("sys/class/accel") {
        if sys::link_name(&format!("sys/class/accel/{a}/device/driver")).as_deref() == Some("intel_vpu") {
            h.npu_kernel = true;
        }
    }
    h.npu_node = sys::exists("dev/accel/accel0");
    h.npu_firmware = !sys::ls("usr/lib/firmware/intel/vpu").is_empty();
    h.npu_userspace = ["usr/lib/libze_intel_npu.so", "usr/lib/libze_intel_npu.so.1", "usr/lib/libze_intel_vpu.so.1",
        "usr/lib/x86_64-linux-gnu/libze_intel_npu.so.1"]
        .iter()
        .any(|p| sys::exists(p));
    h.openvino = sys::exists("usr/lib/libopenvino.so") || sys::exists("usr/lib/libopenvino.so.2026.1.0")
        || sys::ls("usr/lib").iter().any(|n| n.starts_with("libopenvino.so"));

    // power: legacy single-handler path first, then the class device (kernel 6.14+)
    if sys::exists("sys/firmware/acpi/platform_profile_choices") {
        h.profile_path = Some("sys/firmware/acpi/platform_profile".into());
        h.profile_choices = words(&sys::read("sys/firmware/acpi/platform_profile_choices").unwrap_or_default());
    } else if let Some(d) = sys::ls("sys/class/platform-profile").into_iter().next() {
        h.profile_path = Some(format!("sys/class/platform-profile/{d}/profile"));
        h.profile_choices = words(&sys::read(&format!("sys/class/platform-profile/{d}/choices")).unwrap_or_default());
    }
    for s in sys::ls("sys/class/power_supply") {
        let t = sys::read(&format!("sys/class/power_supply/{s}/type")).unwrap_or_default();
        if t == "Battery" && h.battery.is_none() && sys::read(&format!("sys/class/power_supply/{s}/scope")).as_deref() != Some("Device") {
            h.battery = Some(s.clone());
            let ct = sys::read(&format!("sys/class/power_supply/{s}/charge_types"))
                .or_else(|| sys::read(&format!("sys/class/power_supply/{s}/charge_type")))
                .unwrap_or_default();
            h.charge_types = words(&ct).into_iter().map(|w| w.trim_matches(|c| c == '[' || c == ']').to_string()).collect();
            h.charge_thresholds = sys::exists(&format!("sys/class/power_supply/{s}/charge_control_end_threshold"));
        }
        if t == "Mains" && h.ac_supply.is_none() {
            h.ac_supply = Some(s);
        }
    }
    h.epp_choices = words(
        &sys::read("sys/devices/system/cpu/cpufreq/policy0/energy_performance_available_preferences").unwrap_or_default(),
    );
    h
}

pub fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

pub fn report(h: &Hw) -> String {
    let mut o = String::new();
    let mut line = |k: &str, v: String| o.push_str(&format!("{k:<18}: {v}\n"));
    line("machine", format!("{} {} {}", h.vendor, h.product, if h.dell_14_plus { "(Dell 14 Plus: supported)" } else { "" }));
    line("cpu", format!("{}{}", h.cpu, if h.lunar_lake { "  [Lunar Lake / Core Ultra 200V]" } else { "" }));
    line("gpu driver", if h.gpu_driver.is_empty() { "-".into() } else { h.gpu_driver.clone() });
    line(
        "npu",
        format!(
            "kernel driver {} · device node {} · firmware {} · user-space driver {} · OpenVINO {}",
            yn(h.npu_kernel), yn(h.npu_node), yn(h.npu_firmware), yn(h.npu_userspace), yn(h.openvino)
        ),
    );
    line("audio", format!("{} driver {}", h.audio_pci.clone().unwrap_or("-".into()), if h.audio_driver.is_empty() { "none" } else { &h.audio_driver }));
    if !h.codecs.is_empty() {
        let c: Vec<String> = h.codecs.iter().map(|c| format!("{} (link {})", codec_name(c), c.link)).collect();
        line("soundwire codecs", c.join(", "));
    }
    line("thermal modes", if h.profile_choices.is_empty() { "not exposed (dell-pc / platform_profile missing)".into() } else { h.profile_choices.join(" ") });
    line("battery", match &h.battery {
        Some(b) => format!("{b} · charge modes: {} · custom thresholds: {}",
            if h.charge_types.is_empty() { "not exposed".into() } else { h.charge_types.join(" ") }, yn(h.charge_thresholds)),
        None => "none".into(),
    });
    line("cpu energy prefs", if h.epp_choices.is_empty() { "not exposed".into() } else { h.epp_choices.join(" ") });
    o
}
