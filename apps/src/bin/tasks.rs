//! shadow-tasks — task manager: system resources and processes.
//!
//! Cards: CPU (total, per core, frequency) · memory (RAM, swap/zram) · NPU (busy %, frequency, the
//! learning state) · GPU (busy %, frequency) · power (battery draw, package power, time left,
//! temperature) · I/O (disk read/write, network down/up, pressure stall).
//! Below: processes, sorted by CPU, memory or PID.
//!   ↑↓ / wheel select   c cpu · m memory · p pid sort (or click a heading)   / filter
//!   k end process (SIGTERM, asks first)   K kill (SIGKILL, asks first)   Esc / q close
//! Everything is read from /proc and /sys once a second; nothing is sent anywhere.
#[path = "../../../bar/src/sys.rs"]
#[allow(dead_code)]
mod hud;

use shadow_ui::widgets::{chrome, footer, meter, panel, selection, sparkline, Input, Rect};
use shadow_ui::{run, App, Ctx, Frame, Key, Keysym, SurfaceKind};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

const HIST: usize = 90;

fn root() -> PathBuf {
    std::env::var_os("SHADOW_SYS_ROOT").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}
fn rd(rel: &str) -> Option<String> {
    std::fs::read_to_string(root().join(rel.trim_start_matches('/'))).ok()
}
fn num(rel: &str) -> Option<f64> {
    rd(rel)?.trim().parse().ok()
}
fn num_at(r: &std::path::Path, rel: &str) -> Option<f64> {
    std::fs::read_to_string(r.join(rel)).ok()?.trim().parse().ok()
}
fn glob1_at(r: &std::path::Path, dir: &str, prefix: &str, rest: &str) -> Option<String> {
    let mut v: Vec<String> = std::fs::read_dir(r.join(dir)).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with(prefix)).collect();
    v.sort();
    v.into_iter().map(|n| format!("{dir}/{n}/{rest}")).find(|p| r.join(p).exists())
}

/// /proc/stat cpu lines: (busy, total) jiffies for "cpu" and each "cpuN"
pub fn cpu_times(stat: &str) -> Vec<(u64, u64)> {
    stat.lines()
        .filter(|l| l.starts_with("cpu"))
        .map(|l| {
            let v: Vec<u64> = l.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect();
            let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0);
            let total: u64 = v.iter().take(8).sum();
            (total - idle, total)
        })
        .collect()
}

pub fn meminfo(t: &str) -> HashMap<String, u64> {
    t.lines().filter_map(|l| {
        let (k, v) = l.split_once(':')?;
        Some((k.to_string(), v.split_whitespace().next()?.parse().ok()?))
    }).collect()
}

#[derive(Clone, Default)]
struct Proc {
    pid: i32,
    name: String,
    state: char,
    ticks: u64,
    rss_kb: u64,
    uid: u32,
    cpu: f32,
}

fn procs() -> Vec<Proc> {
    let mut v = vec![];
    let Ok(rd_) = std::fs::read_dir(root().join("proc")) else { return v };
    for e in rd_.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
        let Ok(stat) = std::fs::read_to_string(e.path().join("stat")) else { continue };
        // comm may contain spaces and parentheses: split at the last ')'
        let (Some(a), Some(b)) = (stat.find('('), stat.rfind(')')) else { continue };
        let name = stat[a + 1..b].to_string();
        let f: Vec<&str> = stat[b + 2..].split_whitespace().collect();
        let g = |i: usize| f.get(i).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
        let uid = std::fs::read_to_string(e.path().join("status")).ok().and_then(|s| s.lines().find_map(|l| l.strip_prefix("Uid:")).and_then(|u| u.split_whitespace().next()?.parse().ok())).unwrap_or(0);
        v.push(Proc { pid, name, state: f.first().and_then(|s| s.chars().next()).unwrap_or('?'), ticks: g(11) + g(12), rss_kb: g(21) * 4, uid, cpu: 0.0 });
    }
    v
}

fn users() -> HashMap<u32, String> {
    std::fs::read_to_string("/etc/passwd").unwrap_or_default().lines().filter_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        Some((f.get(2)?.parse().ok()?, f.first()?.to_string()))
    }).collect()
}

fn disk_sectors() -> (u64, u64) {
    let t = rd("proc/diskstats").unwrap_or_default();
    let mut r = (0, 0);
    for l in t.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        let name = f.get(2).copied().unwrap_or("");
        // whole disks only (nvme0n1, sda, mmcblk0), not partitions or loop/ram/zram/dm
        let whole = (name.starts_with("nvme") && !name.contains('p')) || (name.starts_with("sd") && name.len() == 3) || (name.starts_with("mmcblk") && !name.contains('p')) || name.starts_with("vd") && name.len() == 3;
        if whole {
            r.0 += f.get(5).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
            r.1 += f.get(9).and_then(|x| x.parse::<u64>().ok()).unwrap_or(0);
        }
    }
    r
}

fn net_bytes() -> (u64, u64) {
    let t = rd("proc/net/dev").unwrap_or_default();
    let mut r = (0, 0);
    for l in t.lines().skip(2) {
        let Some((iface, rest)) = l.split_once(':') else { continue };
        if iface.trim() == "lo" {
            continue;
        }
        let f: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        r.0 += f.first().copied().unwrap_or(0);
        r.1 += f.get(8).copied().unwrap_or(0);
    }
    r
}

fn psi(kind: &str) -> Option<f32> {
    rd(&format!("proc/pressure/{kind}"))?.lines().next()?.split_whitespace().find_map(|x| x.strip_prefix("avg10="))?.parse().ok()
}

/// Intel NPU (intel_vpu): accumulated busy time in µs, current / max frequency in MHz
fn npu() -> Option<(u64, Option<f64>, Option<f64>)> {
    npu_at(&root())
}
fn npu_at(r: &std::path::Path) -> Option<(u64, Option<f64>, Option<f64>)> {
    let busy = glob1_at(r, "sys/class/accel", "accel", "device/npu_busy_time_us")?;
    let dir = busy.trim_end_matches("npu_busy_time_us").to_string();
    Some((num_at(r, &busy)? as u64, num_at(r, &format!("{dir}npu_current_frequency_mhz")), num_at(r, &format!("{dir}npu_max_frequency_mhz"))))
}

/// GPU (xe, else i915): idle residency in ms, actual frequency in MHz
fn gpu() -> Option<(u64, Option<f64>, &'static str)> {
    gpu_at(&root())
}
fn gpu_at(r: &std::path::Path) -> Option<(u64, Option<f64>, &'static str)> {
    if let Some(p) = glob1_at(r, "sys/class/drm", "card", "device/tile0/gt0/gtidle/idle_residency_ms") {
        let f = p.replace("gtidle/idle_residency_ms", "freq0/act_freq");
        return Some((num_at(r, &p)? as u64, num_at(r, &f), "xe"));
    }
    let p = glob1_at(r, "sys/class/drm", "card", "gt/gt0/rc6_residency_ms")?;
    let f = p.replace("gt/gt0/rc6_residency_ms", "gt_act_freq_mhz");
    Some((num_at(r, &p)? as u64, num_at(r, &f), "i915"))
}

fn rapl_uj() -> Option<u64> {
    num("sys/class/powercap/intel-rapl:0/energy_uj").map(|v| v as u64)
}

#[derive(Default)]
struct Sample {
    at: Option<Instant>,
    cpu: Vec<(u64, u64)>,
    proc_ticks: HashMap<i32, u64>,
    disk: (u64, u64),
    net: (u64, u64),
    npu_us: Option<u64>,
    gpu_idle: Option<u64>,
    rapl: Option<u64>,
}

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Cpu,
    Mem,
    Pid,
}

struct Tasks {
    prev: Sample,
    cpu_total: f32,
    cores: Vec<f32>,
    freq_mhz: f64,
    cpu_hist: VecDeque<f32>,
    mem: (u64, u64, u64, u64), // total, available, swap total, swap free (kB)
    mem_hist: VecDeque<f32>,
    npu: Option<(f32, Option<f64>, Option<f64>)>,
    npu_hist: VecDeque<f32>,
    npu_state: String,
    gpu: Option<(f32, Option<f64>, &'static str)>,
    gpu_hist: VecDeque<f32>,
    watts_bat: Option<f64>,
    watts_pkg: Option<f64>,
    temp: Option<i32>,
    bat: Option<u8>,
    left: Option<(u32, bool)>,
    disk_rate: (f64, f64),
    net_rate: (f64, f64),
    psi: (Option<f32>, Option<f32>, Option<f32>),
    load: String,
    procs: Vec<Proc>,
    users: HashMap<u32, String>,
    sort: Sort,
    sel: usize,
    top: usize,
    visible: usize,
    filter: Input,
    filtering: bool,
    confirm: Option<(i32, String, i32)>,
    msg: (String, bool),
    heads: Vec<(Sort, Rect)>,
    rows: Vec<(usize, Rect)>,
    clk_tck: f64,
}

fn push(h: &mut VecDeque<f32>, v: f32) {
    if h.len() >= HIST {
        h.pop_front();
    }
    h.push_back(v.clamp(0.0, 1.0));
}

fn human(bytes_per_s: f64) -> String {
    let b = bytes_per_s.max(0.0);
    if b >= 1e9 {
        format!("{:.1} GB/s", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB/s", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} kB/s", b / 1e3)
    } else {
        format!("{b:.0} B/s")
    }
}

impl Tasks {
    fn sample(&mut self) {
        let now = Instant::now();
        let dt = self.prev.at.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        // CPU
        let cpu = cpu_times(&rd("proc/stat").unwrap_or_default());
        if !self.prev.cpu.is_empty() && self.prev.cpu.len() == cpu.len() {
            let pct = |a: (u64, u64), b: (u64, u64)| {
                let dt = b.1.saturating_sub(a.1) as f32;
                if dt > 0.0 { b.0.saturating_sub(a.0) as f32 / dt } else { 0.0 }
            };
            self.cpu_total = pct(self.prev.cpu[0], cpu[0]);
            self.cores = self.prev.cpu.iter().zip(&cpu).skip(1).map(|(a, b)| pct(*a, *b)).collect();
            push(&mut self.cpu_hist, self.cpu_total);
        }
        let freqs: Vec<f64> = (0..cpu.len().saturating_sub(1)).filter_map(|i| num(&format!("sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq"))).collect();
        self.freq_mhz = if freqs.is_empty() { 0.0 } else { freqs.iter().sum::<f64>() / freqs.len() as f64 / 1000.0 };
        // memory
        let m = meminfo(&rd("proc/meminfo").unwrap_or_default());
        let g = |k: &str| m.get(k).copied().unwrap_or(0);
        self.mem = (g("MemTotal"), g("MemAvailable"), g("SwapTotal"), g("SwapFree"));
        if self.mem.0 > 0 {
            push(&mut self.mem_hist, 1.0 - self.mem.1 as f32 / self.mem.0 as f32);
        }
        // NPU
        let n = npu();
        self.npu = match (n, self.prev.npu_us) {
            (Some((us, cur, max)), Some(p)) if dt > 0.0 => Some((((us.saturating_sub(p)) as f64 / (dt * 1e6)).clamp(0.0, 1.0) as f32, cur, max)),
            (Some((_, cur, max)), _) => Some((self.npu.map(|x| x.0).unwrap_or(0.0), cur, max)),
            _ => None,
        };
        if let Some((b, ..)) = self.npu {
            push(&mut self.npu_hist, b);
        }
        let ns = hud::npu_state();
        self.npu_state = if let Some(r) = hud::npu_reason() { format!("{ns} · {}", r.replace('-', " ")) } else { ns };
        // GPU
        let gp = gpu();
        self.gpu = match (gp, self.prev.gpu_idle) {
            (Some((idle, f, drv)), Some(p)) if dt > 0.0 => Some(((1.0 - idle.saturating_sub(p) as f64 / (dt * 1000.0)).clamp(0.0, 1.0) as f32, f, drv)),
            (Some((_, f, drv)), _) => Some((self.gpu.map(|x| x.0).unwrap_or(0.0), f, drv)),
            _ => None,
        };
        if let Some((b, ..)) = self.gpu {
            push(&mut self.gpu_hist, b);
        }
        // power
        let p = hud::power();
        self.watts_bat = p.watts.filter(|_| !p.ac || p.charging);
        self.bat = p.battery;
        self.left = hud::time_left(&p, p.watts);
        self.temp = hud::package_temp();
        let rapl = rapl_uj();
        self.watts_pkg = match (rapl, self.prev.rapl) {
            (Some(a), Some(b)) if dt > 0.0 && a >= b => Some((a - b) as f64 / 1e6 / dt),
            _ => None,
        };
        // I/O
        let disk = disk_sectors();
        let net = net_bytes();
        if dt > 0.0 {
            self.disk_rate = (disk.0.saturating_sub(self.prev.disk.0) as f64 * 512.0 / dt, disk.1.saturating_sub(self.prev.disk.1) as f64 * 512.0 / dt);
            self.net_rate = (net.0.saturating_sub(self.prev.net.0) as f64 / dt, net.1.saturating_sub(self.prev.net.1) as f64 / dt);
        }
        self.psi = (psi("cpu"), psi("memory"), psi("io"));
        self.load = rd("proc/loadavg").unwrap_or_default().split_whitespace().take(3).collect::<Vec<_>>().join(" ");
        // processes
        let mut ps = procs();
        let mut ticks = HashMap::new();
        for pr in &mut ps {
            if let Some(old) = self.prev.proc_ticks.get(&pr.pid) {
                if dt > 0.0 {
                    pr.cpu = (pr.ticks.saturating_sub(*old) as f64 / self.clk_tck / dt * 100.0) as f32;
                }
            }
            ticks.insert(pr.pid, pr.ticks);
        }
        let selected = self.shown().get(self.sel).map(|p| p.pid);
        self.procs = ps;
        self.order();
        if let Some(pid) = selected {
            if let Some(i) = self.shown().iter().position(|p| p.pid == pid) {
                self.sel = i;
            }
        }
        self.prev = Sample { at: Some(now), cpu, proc_ticks: ticks, disk, net, npu_us: n.map(|x| x.0), gpu_idle: gp.map(|x| x.0), rapl };
    }

    fn order(&mut self) {
        match self.sort {
            Sort::Cpu => self.procs.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal).then(b.rss_kb.cmp(&a.rss_kb))),
            Sort::Mem => self.procs.sort_by(|a, b| b.rss_kb.cmp(&a.rss_kb)),
            Sort::Pid => self.procs.sort_by_key(|p| p.pid),
        }
    }

    fn shown(&self) -> Vec<&Proc> {
        let q = self.filter.text.to_lowercase();
        self.procs.iter().filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q) || p.pid.to_string() == q).collect()
    }

    fn clamp(&mut self) {
        let n = self.shown().len();
        self.sel = self.sel.min(n.saturating_sub(1));
        let v = self.visible.max(1);
        if self.sel < self.top {
            self.top = self.sel;
        }
        if self.sel >= self.top + v {
            self.top = self.sel + 1 - v;
        }
    }

    fn ask_kill(&mut self, sig: i32) {
        if let Some(p) = self.shown().get(self.sel) {
            self.confirm = Some((p.pid, p.name.clone(), sig));
        }
    }
}

fn card(f: &mut Frame, r: Rect, title: &str, value: &str, sub: &str, hist: Option<&VecDeque<f32>>, accent: shadow_ui::Rgb) {
    let th = f.theme.clone();
    panel(f, r.x, r.y, r.w, r.h, accent);
    let pad = f.px(10.0);
    let lpt = th.font_pt * 0.78;
    let vpt = th.font_pt * 1.35;
    f.text(r.x + pad, r.y + pad, title, lpt, th.dim, true);
    let vy = r.y + pad + f.line_h(lpt) + f.px(2.0);
    let v = f.fit(value, vpt, true, r.w - 2 * pad);
    f.text(r.x + pad, vy, &v, vpt, accent, true);
    let sy = vy + f.line_h(vpt);
    let s = f.fit(sub, lpt, false, r.w - 2 * pad);
    f.text(r.x + pad, sy, &s, lpt, th.text, false);
    if let Some(h) = hist {
        let v: Vec<f32> = h.iter().copied().collect();
        let gy = sy + f.line_h(lpt) + f.px(4.0);
        let gh = r.y + r.h - pad - gy;
        if gh > f.px(8.0) {
            sparkline(f, r.x + pad, gy, r.w - 2 * pad, gh, &v, accent);
        }
    }
}

impl App for Tasks {
    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Window { app_id: "shadow-tasks".into(), title: "Task manager".into(), size: (980, 700) }
    }

    fn tick_ms(&self) -> u64 {
        1000
    }

    fn tick(&mut self, ctx: &mut Ctx) {
        self.sample();
        self.clamp();
        ctx.redraw();
    }

    fn key(&mut self, k: &Key, ctx: &mut Ctx) {
        if let Some((pid, _, sig)) = self.confirm.clone() {
            match k.sym {
                Keysym::y | Keysym::Return => {
                    // SAFETY: kill(2) with a pid we listed; failures are reported, not fatal
                    let r = unsafe { libc::kill(pid, sig) };
                    self.msg = if r == 0 {
                        (format!("sent {} to {pid}", if sig == libc::SIGKILL { "SIGKILL" } else { "SIGTERM" }), false)
                    } else {
                        (format!("could not signal {pid}: {} (another user's process needs sudo)", std::io::Error::last_os_error()), true)
                    };
                }
                _ => self.msg = ("cancelled".into(), false),
            }
            self.confirm = None;
            ctx.redraw();
            return;
        }
        if self.filtering {
            match k.sym {
                Keysym::Escape | Keysym::Return => self.filtering = false,
                _ => {
                    self.filter.key(k);
                    self.sel = 0;
                    self.top = 0;
                }
            }
            ctx.redraw();
            return;
        }
        let n = self.shown().len();
        match k.sym {
            Keysym::Escape | Keysym::q => {
                if !self.filter.text.is_empty() {
                    self.filter.clear();
                } else {
                    ctx.quit();
                }
            }
            Keysym::Up => self.sel = self.sel.saturating_sub(1),
            Keysym::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            Keysym::Page_Up => self.sel = self.sel.saturating_sub(self.visible),
            Keysym::Page_Down => self.sel = (self.sel + self.visible).min(n.saturating_sub(1)),
            Keysym::c => self.sort = Sort::Cpu,
            Keysym::m => self.sort = Sort::Mem,
            Keysym::p => self.sort = Sort::Pid,
            Keysym::slash => self.filtering = true,
            Keysym::k => self.ask_kill(libc::SIGTERM),
            Keysym::K => self.ask_kill(libc::SIGKILL),
            _ => return,
        }
        self.order();
        self.clamp();
        ctx.redraw();
    }

    fn click(&mut self, x: i32, y: i32, ctx: &mut Ctx) {
        if let Some(&(s, _)) = self.heads.iter().find(|(_, r)| r.hit(x, y)) {
            self.sort = s;
            self.order();
        } else if let Some(&(i, _)) = self.rows.iter().find(|(_, r)| r.hit(x, y)) {
            self.sel = i;
        } else {
            return;
        }
        ctx.redraw();
    }

    fn scroll(&mut self, lines: i32, ctx: &mut Ctx) {
        let n = self.shown().len() as i32;
        self.sel = (self.sel as i32 + lines).clamp(0, (n - 1).max(0)) as usize;
        self.clamp();
        ctx.redraw();
    }

    fn draw(&mut self, f: &mut Frame) {
        let th = f.theme.clone();
        let status = format!("load {} · {} processes", self.load, self.procs.len());
        let (top, pad) = chrome(f, "TASKS", &status);
        let (w, h) = (f.cv.w as i32, f.cv.h as i32);
        let pt = th.font_pt * 0.92;
        let lh = f.line_h(pt);
        // ── cards: 3 × 2
        let gap = f.px(10.0);
        let cw = (w - 2 * pad - 2 * gap) / 3;
        let chh = f.px(118.0);
        let at = |i: i32, j: i32| Rect { x: pad + i * (cw + gap), y: top + j * (chh + gap), w: cw, h: chh };
        let gib = |kb: u64| kb as f64 / 1048576.0;
        // CPU
        let cores = self.cores.len();
        card(f, at(0, 0), "CPU", &format!("{:.0} %", self.cpu_total * 100.0), &format!("{cores} cores{}", if self.freq_mhz > 0.0 { format!(" · {:.2} GHz", self.freq_mhz / 1000.0) } else { String::new() }), Some(&self.cpu_hist), th.cyan);
        // per-core strip inside the CPU card, right side
        {
            let r = at(0, 0);
            let n = self.cores.len().max(1) as i32;
            let bw = ((r.w / 2 - f.px(10.0)) / n).clamp(2, f.px(10.0));
            let x0 = r.x + r.w - f.px(10.0) - n * bw;
            let bh = f.px(36.0);
            for (i, c) in self.cores.iter().enumerate() {
                let hh = ((bh as f32) * c).round() as i32;
                let x = x0 + i as i32 * bw;
                f.cv.fill(x, r.y + f.px(12.0), bw - 1, bh, th.glass, 1.0);
                let col = shadow_ui::canvas::lerp(th.cyan, th.amber, *c);
                f.cv.fill(x, r.y + f.px(12.0) + bh - hh, bw - 1, hh, col, 0.9);
            }
        }
        // memory
        let (tot, avail, st, sf) = self.mem;
        let used = tot.saturating_sub(avail);
        card(f, at(1, 0), "MEMORY", &format!("{:.1} / {:.1} GiB", gib(used), gib(tot)), &format!("swap {:.1} / {:.1} GiB{}", gib(st.saturating_sub(sf)), gib(st), self.psi.1.map(|p| format!(" · stall {p:.1} %")).unwrap_or_default()), Some(&self.mem_hist), th.violet);
        // NPU
        match self.npu {
            Some((b, cur, max)) => card(f, at(2, 0), "NPU", &format!("{:.0} %", b * 100.0), &format!("{}{} · {}", cur.map(|c| format!("{c:.0} MHz")).unwrap_or_else(|| "busy".into()), max.map(|m| format!(" / {m:.0}")).unwrap_or_default(), self.npu_state), Some(&self.npu_hist), th.amber),
            None => card(f, at(2, 0), "NPU", "—", &format!("no intel_vpu busy counter · {}", self.npu_state), None, th.dim),
        }
        // GPU
        match self.gpu {
            Some((b, fr, drv)) => card(f, at(0, 1), "GPU", &format!("{:.0} %", b * 100.0), &format!("{drv}{}", fr.map(|x| format!(" · {x:.0} MHz")).unwrap_or_default()), Some(&self.gpu_hist), th.green),
            None => card(f, at(0, 1), "GPU", "—", "no xe / i915 residency counter", None, th.dim),
        }
        // power
        let watts = match (self.watts_bat, self.watts_pkg) {
            (Some(b), Some(p)) => format!("{b:.1} W · pkg {p:.1} W"),
            (Some(b), None) => format!("{b:.1} W"),
            (None, Some(p)) => format!("pkg {p:.1} W"),
            _ => "AC".into(),
        };
        let left = match self.left {
            Some((m, false)) => format!("{} left", hud::fmt_minutes(m)),
            Some((m, true)) => format!("{} to full", hud::fmt_minutes(m)),
            None => "on AC".into(),
        };
        card(f, at(1, 1), "POWER", &watts, &format!("{}{}{}", self.bat.map(|b| format!("{b}% · ")).unwrap_or_default(), left, self.temp.map(|t| format!(" · {t}°C")).unwrap_or_default()), None, th.amber);
        if let Some(b) = self.bat {
            let r = at(1, 1);
            meter(f, r.x + f.px(10.0), r.y + r.h - f.px(18.0), r.w - f.px(20.0), f.px(6.0), b as f32 / 100.0, false);
        }
        // I/O
        card(f, at(2, 1), "I/O", &format!("↓ {}", human(self.net_rate.0)), &format!("↑ {} · disk r {} w {}", human(self.net_rate.1), human(self.disk_rate.0), human(self.disk_rate.1)), None, th.cyan);
        {
            let r = at(2, 1);
            let s = format!("pressure  cpu {} · io {}", self.psi.0.map(|p| format!("{p:.1}%")).unwrap_or("-".into()), self.psi.2.map(|p| format!("{p:.1}%")).unwrap_or("-".into()));
            let s = f.fit(&s, th.font_pt * 0.78, false, r.w - f.px(20.0));
            f.text(r.x + f.px(10.0), r.y + r.h - f.px(24.0), &s, th.font_pt * 0.78, th.dim, false);
        }
        // ── processes
        let ty = top + 2 * (chh + gap) + f.px(4.0);
        let cols = [("PID", Sort::Pid, f.px(70.0)), ("NAME", Sort::Pid, 0), ("USER", Sort::Pid, f.px(110.0)), ("CPU %", Sort::Cpu, f.px(80.0)), ("MEMORY", Sort::Mem, f.px(100.0)), ("S", Sort::Pid, f.px(30.0))];
        let fixed: i32 = cols.iter().map(|c| c.2).sum();
        let namew = w - 2 * pad - fixed - f.px(20.0);
        let mut xs = vec![];
        let mut x = pad + f.px(18.0);
        for c in cols.iter() {
            xs.push(x);
            x += if c.2 == 0 { namew } else { c.2 };
        }
        self.heads.clear();
        for (i, (name, s, _)) in cols.iter().enumerate() {
            let active = (*s == self.sort) && (i == 0 && self.sort == Sort::Pid || i == 3 || i == 4);
            let label = if active { format!("{name} ▾") } else { name.to_string() };
            f.text(xs[i], ty, &label, th.font_pt * 0.78, if active { th.amber } else { th.dim }, true);
            if i == 0 || i == 3 || i == 4 {
                self.heads.push((*s, Rect { x: xs[i], y: ty, w: f.px(80.0), h: lh }));
            }
        }
        let hlh = f.line_h(th.font_pt * 0.78);
        let by = ty + hlh + f.px(3.0);
        f.cv.beam(pad, by, w - 2 * pad, 1, th.amber, th.cyan, 0.5);
        let row_h = lh + f.px(6.0);
        let list_top = ty + f.line_h(th.font_pt * 0.78) + f.px(8.0);
        let foot = f.line_h(th.font_pt * 0.88) + pad + f.px(14.0) + if self.confirm.is_some() || !self.msg.0.is_empty() || self.filtering || !self.filter.text.is_empty() { lh + f.px(8.0) } else { 0 };
        self.visible = ((h - list_top - foot) / row_h).max(1) as usize;
        self.clamp();
        self.rows.clear();
        let shown: Vec<Proc> = self.shown().into_iter().cloned().collect();
        for (i, p) in shown.iter().enumerate().skip(self.top).take(self.visible) {
            let y = list_top + (i - self.top) as i32 * row_h;
            let r = Rect { x: pad, y, w: w - 2 * pad, h: row_h - f.px(2.0) };
            if i == self.sel {
                selection(f, r.x, r.y, r.w, r.h);
            }
            let ty2 = y + f.px(2.0);
            let user = self.users.get(&p.uid).cloned().unwrap_or_else(|| p.uid.to_string());
            let mem = if p.rss_kb >= 1048576 { format!("{:.1} GiB", gib(p.rss_kb)) } else { format!("{:.0} MiB", p.rss_kb as f64 / 1024.0) };
            let hot = p.cpu >= 50.0;
            let cells = [p.pid.to_string(), p.name.clone(), user, format!("{:.1}", p.cpu), mem, p.state.to_string()];
            for (k, c) in cells.iter().enumerate() {
                let maxw = if k == 1 { namew - f.px(8.0) } else { cols[k].2 - f.px(6.0) };
                let s = f.fit(c, pt, false, maxw);
                let col = match k {
                    3 if hot => th.amber,
                    3 => th.cyan,
                    1 => th.text,
                    _ => th.dim,
                };
                f.text(xs[k], ty2, &s, pt, col, i == self.sel && k == 1);
            }
            self.rows.push((i, r));
        }
        // prompt / message line
        let my = h - foot + f.px(4.0);
        if let Some((pid, name, sig)) = &self.confirm {
            let s = format!("{} {name} ({pid})?  y = yes · any other key = no", if *sig == libc::SIGKILL { "KILL (SIGKILL)" } else { "End (SIGTERM)" });
            f.text(pad + f.px(10.0), my, &s, pt, th.crimson, true);
        } else if self.filtering || !self.filter.text.is_empty() {
            let s = f.text(pad + f.px(10.0), my, "filter: ", pt, th.amber, true);
            self.filter.draw(f, pad + f.px(10.0) + s, my, pt, self.filtering);
        } else if !self.msg.0.is_empty() {
            f.text(pad + f.px(10.0), my, &self.msg.0, pt, if self.msg.1 { th.crimson } else { th.cyan }, false);
        }
        footer(f, "↑↓ select · c cpu · m memory · p pid (or click a heading) · / filter · k end · K kill · Esc close");
    }
}

fn main() {
    // SAFETY: sysconf has no preconditions
    let tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    let mut t = Tasks {
        prev: Sample::default(),
        cpu_total: 0.0,
        cores: vec![],
        freq_mhz: 0.0,
        cpu_hist: VecDeque::new(),
        mem: (0, 0, 0, 0),
        mem_hist: VecDeque::new(),
        npu: None,
        npu_hist: VecDeque::new(),
        npu_state: String::new(),
        gpu: None,
        gpu_hist: VecDeque::new(),
        watts_bat: None,
        watts_pkg: None,
        temp: None,
        bat: None,
        left: None,
        disk_rate: (0.0, 0.0),
        net_rate: (0.0, 0.0),
        psi: (None, None, None),
        load: String::new(),
        procs: vec![],
        users: users(),
        sort: Sort::Cpu,
        sel: 0,
        top: 0,
        visible: 10,
        filter: Input::default(),
        filtering: false,
        confirm: None,
        msg: (String::new(), false),
        heads: vec![],
        rows: vec![],
        clk_tck: if tck > 0.0 { tck } else { 100.0 },
    };
    t.sample();
    if let Err(e) = run(Box::new(t), None) {
        eprintln!("shadow-tasks: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_and_memory_parsing() {
        let a = cpu_times("cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 50 0 50 400 0 0 0 0\n");
        assert_eq!(a, vec![(200, 1000), (100, 500)]);
        let m = meminfo("MemTotal:       16000000 kB\nMemAvailable:    4000000 kB\n");
        assert_eq!(m["MemAvailable"], 4000000);
    }

    #[test]
    fn npu_and_gpu_counters_from_sysfs() {
        let r = std::env::temp_dir().join(format!("tasks-{}", std::process::id()));
        let w = |p: &str, v: &str| {
            let f = r.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, v).unwrap();
        };
        w("sys/class/accel/accel0/device/npu_busy_time_us", "1500000\n");
        w("sys/class/accel/accel0/device/npu_current_frequency_mhz", "1400\n");
        w("sys/class/drm/card0/device/tile0/gt0/gtidle/idle_residency_ms", "900\n");
        w("sys/class/drm/card0/device/tile0/gt0/freq0/act_freq", "1950\n");
        assert_eq!(npu_at(&r).map(|n| (n.0, n.1)), Some((1500000, Some(1400.0))));
        assert_eq!(gpu_at(&r).map(|g| (g.0, g.1, g.2)), Some((900, Some(1950.0), "xe")));
        let _ = std::fs::remove_dir_all(&r);
    }
}
