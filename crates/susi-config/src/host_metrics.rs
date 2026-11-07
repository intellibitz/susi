//! Host-wide CPU, memory, load, uptime, identity and filesystem readings on
//! the platforms core ships on (x86_64 Linux and Windows), with nothing but
//! the standard library and the kernel's own records: `/proc` and `/sys` on
//! Linux, Win32 through the `winapi` facade's `host` module on Windows.
//!
//! Until 2026-11-07 these came through the `sysinfo` facade, which put the
//! 85-second `windows` crate on the critical path of every core build. A
//! metric an OS does not have (a load average on Windows) answers `None`,
//! and [`load_average_unavailable`] names why, so callers report
//! "unavailable" instead of printing a made-up zero. Any other target
//! answers `None` or empty everywhere.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Whether this target has a host-metrics source at all.
pub const IS_SUPPORTED_SYSTEM: bool = cfg!(any(target_os = "linux", windows));

/// Shortest interval between two CPU readings that yields a meaningful
/// utilization delta.
pub const MINIMUM_CPU_UPDATE_INTERVAL: Duration = Duration::from_millis(200);

/// Physical and swap memory, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// Installed physical memory.
    pub total_bytes: u64,
    /// Memory available to new work without swapping (Linux `MemAvailable`,
    /// the Windows "available" figure).
    pub available_bytes: u64,
    /// Swap or page file size beyond physical memory.
    pub total_swap_bytes: u64,
    /// Unused swap.
    pub free_swap_bytes: u64,
}

impl MemorySnapshot {
    /// Memory in use (total minus available).
    pub fn used_bytes(&self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }

    /// Share of physical memory in use, 0-100.
    pub fn used_pct(&self) -> f64 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.used_bytes() as f64 / self.total_bytes as f64 * 100.0).clamp(0.0, 100.0)
    }
}

/// Logical CPUs visible to this process.
pub fn cpu_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Current physical and swap memory, or `None` when this OS reports none.
pub fn memory() -> Option<MemorySnapshot> {
    platform::memory()
}

/// The 1, 5 and 15 minute load averages. `None` on Windows, which keeps no
/// load average (see [`cpu_util_pct`] there), and on unsupported targets.
pub fn load_average() -> Option<[f64; 3]> {
    if load_average_unavailable().is_some() {
        return None;
    }
    platform::load_average()
}

/// Why [`load_average`] answers `None` on this OS, or `None` when it reads.
pub fn load_average_unavailable() -> Option<&'static str> {
    if cfg!(windows) {
        Some("Windows keeps no load average; use the sampled CPU utilization instead")
    } else if !IS_SUPPORTED_SYSTEM {
        Some("this OS has no supported load-average source")
    } else {
        None
    }
}

/// Cumulative CPU ticks since boot, summed over every logical CPU, in any
/// unit as long as both fields share it: utilization is the busy share of
/// the delta between two readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuTicks {
    busy: u64,
    total: u64,
}

/// One process-wide CPU sampler. CPU utilization is a delta between two
/// readings, so the sampler keeps its last reading: a long-lived caller (the
/// daemon's governor) gets the utilization since its previous call with no
/// wait, and only the very first call in a process waits one minimum
/// sampling interval.
struct CpuSampler {
    last: CpuTicks,
    taken: Instant,
    primed: bool,
    last_pct: f32,
}

fn cpu_sampler() -> &'static Mutex<CpuSampler> {
    static SAMPLER: OnceLock<Mutex<CpuSampler>> = OnceLock::new();
    SAMPLER.get_or_init(|| {
        Mutex::new(CpuSampler {
            last: platform::cpu_ticks().unwrap_or(CpuTicks { busy: 0, total: 0 }),
            taken: Instant::now(),
            primed: false,
            last_pct: 0.0,
        })
    })
}

/// Host-wide CPU utilization in percent (0-100), averaged over every logical
/// CPU since the previous call (or over one minimum sampling interval on the
/// first call). `None` on unsupported targets.
pub fn cpu_util_pct() -> Option<f32> {
    if !IS_SUPPORTED_SYSTEM {
        return None;
    }
    let mut sampler = cpu_sampler().lock().unwrap_or_else(|e| e.into_inner());
    let elapsed = sampler.taken.elapsed();
    if sampler.primed && elapsed < MINIMUM_CPU_UPDATE_INTERVAL {
        return Some(sampler.last_pct);
    }
    if !sampler.primed && elapsed < MINIMUM_CPU_UPDATE_INTERVAL {
        std::thread::sleep(MINIMUM_CPU_UPDATE_INTERVAL - elapsed);
    }
    let now = platform::cpu_ticks()?;
    let busy = now.busy.saturating_sub(sampler.last.busy);
    let total = now.total.saturating_sub(sampler.last.total);
    let pct = if total == 0 {
        sampler.last_pct
    } else {
        ((busy as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as f32
    };
    sampler.last = now;
    sampler.taken = Instant::now();
    sampler.primed = true;
    sampler.last_pct = pct;
    Some(pct)
}

/// CPU pressure in percent (0-100) for governors and the `top` view: the
/// 1-minute load average per logical CPU where the OS keeps one (Linux),
/// the sampled utilization on Windows. `None` when neither exists.
pub fn cpu_pressure_pct() -> Option<f32> {
    match load_average() {
        Some([one, _, _]) => Some(((one / cpu_count() as f64) * 100.0).clamp(0.0, 100.0) as f32),
        None => cpu_util_pct(),
    }
}

/// Seconds since boot, or `None` when this OS reports none.
pub fn uptime_secs() -> Option<u64> {
    platform::uptime_secs().filter(|secs| *secs > 0)
}

/// The CPU model string ("AMD Ryzen 9 7950X 16-Core Processor"), when the
/// OS names one.
pub fn cpu_brand() -> Option<String> {
    platform::cpu_brand()
        .map(|brand| brand.trim().to_string())
        .filter(|brand| !brand.is_empty())
}

/// Temperature sensors the OS exposes, as `(label, degrees Celsius)`. Linux
/// reads `hwmon` and the thermal zones; Windows exposes none to an
/// unelevated process. Sensors without a reading are skipped, so an empty
/// list means "none readable here".
pub fn temperature_sensors() -> Vec<(String, f32)> {
    platform::temperature_sensors()
        .into_iter()
        .filter(|(_, celsius)| celsius.is_finite() && *celsius > 0.0)
        .collect()
}

/// The kernel family name: `Linux`, `Darwin`, `Windows NT`, or the target OS.
pub fn kernel_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Darwin",
        "windows" => "Windows NT",
        other => other,
    }
}

/// The kernel release ("6.8.0-45-generic", the Windows build number), when
/// the OS reports it.
pub fn kernel_version() -> Option<String> {
    platform::kernel_version().filter(|v| !v.trim().is_empty())
}

/// The human OS name and version ("Ubuntu 24.04 LTS", "Windows 11 Pro
/// 24H2"), when the OS reports it.
pub fn long_os_version() -> Option<String> {
    platform::long_os_version().filter(|v| !v.trim().is_empty())
}

/// This host's name as the OS reports it, or empty when it reports none.
pub fn host_name() -> String {
    platform::host_name()
        .map(|name| name.trim().to_string())
        .unwrap_or_default()
}

/// One mounted filesystem backed by a real device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filesystem {
    /// Where it is mounted (`/`, `/home`, `C:\`).
    pub mount_point: std::path::PathBuf,
    /// The filesystem type (`ext4`, `NTFS`).
    pub file_system: String,
    /// Size in bytes.
    pub total_bytes: u64,
    /// Bytes available to this process.
    pub available_bytes: u64,
}

/// Every device-backed filesystem with its size and free space, in mount
/// order. Empty on unsupported targets.
pub fn filesystems() -> Vec<Filesystem> {
    platform::filesystems()
}

/// The filesystem holding `path`: the longest mount point that prefixes it.
pub fn filesystem_of(path: &std::path::Path) -> Option<Filesystem> {
    filesystems()
        .into_iter()
        .filter(|fs| path.starts_with(&fs.mount_point))
        .max_by_key(|fs| fs.mount_point.as_os_str().len())
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{CpuTicks, Filesystem, MemorySnapshot};
    use std::path::PathBuf;

    fn read(path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    /// `key:   1234 kB` lines of `/proc/meminfo`, in bytes.
    fn meminfo_bytes(text: &str, key: &str) -> Option<u64> {
        text.lines().find_map(|line| {
            let rest = line.strip_prefix(key)?.strip_prefix(':')?;
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            Some(kb * 1024)
        })
    }

    pub(super) fn memory() -> Option<MemorySnapshot> {
        let text = read("/proc/meminfo")?;
        let total_bytes = meminfo_bytes(&text, "MemTotal")?;
        Some(MemorySnapshot {
            total_bytes,
            available_bytes: meminfo_bytes(&text, "MemAvailable")
                .unwrap_or_else(|| meminfo_bytes(&text, "MemFree").unwrap_or(0))
                .min(total_bytes),
            total_swap_bytes: meminfo_bytes(&text, "SwapTotal").unwrap_or(0),
            free_swap_bytes: meminfo_bytes(&text, "SwapFree").unwrap_or(0),
        })
    }

    pub(super) fn load_average() -> Option<[f64; 3]> {
        let text = read("/proc/loadavg")?;
        let mut fields = text.split_whitespace().map(|f| f.parse::<f64>().ok());
        Some([fields.next()??, fields.next()??, fields.next()??])
    }

    /// The aggregate `cpu` line of `/proc/stat`: user nice system idle
    /// iowait irq softirq steal. Idle time is idle plus iowait.
    pub(super) fn cpu_ticks() -> Option<CpuTicks> {
        let text = read("/proc/stat")?;
        let line = text.lines().find(|l| l.starts_with("cpu "))?;
        let fields: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .filter_map(|f| f.parse().ok())
            .collect();
        if fields.len() < 4 {
            return None;
        }
        let total: u64 = fields.iter().take(8).sum();
        let idle = fields[3] + fields.get(4).copied().unwrap_or(0);
        Some(CpuTicks {
            busy: total.saturating_sub(idle),
            total,
        })
    }

    pub(super) fn uptime_secs() -> Option<u64> {
        let text = read("/proc/uptime")?;
        let secs: f64 = text.split_whitespace().next()?.parse().ok()?;
        Some(secs as u64)
    }

    pub(super) fn cpu_brand() -> Option<String> {
        let text = read("/proc/cpuinfo")?;
        text.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key.trim() == "model name").then(|| value.trim().to_string())
        })
    }

    pub(super) fn temperature_sensors() -> Vec<(String, f32)> {
        let mut sensors = Vec::new();
        if let Ok(hwmons) = std::fs::read_dir("/sys/class/hwmon") {
            for hwmon in hwmons.flatten() {
                let dir = hwmon.path();
                let chip = read(&dir.join("name").to_string_lossy())
                    .map(|n| n.trim().to_string())
                    .unwrap_or_else(|| hwmon.file_name().to_string_lossy().into_owned());
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                let mut inputs: Vec<String> = entries
                    .flatten()
                    .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                    .filter(|n| n.starts_with("temp") && n.ends_with("_input"))
                    .collect();
                inputs.sort();
                for input in inputs {
                    let stem = input.trim_end_matches("_input");
                    let Some(milli) = read(&dir.join(&input).to_string_lossy())
                        .and_then(|v| v.trim().parse::<f32>().ok())
                    else {
                        continue;
                    };
                    let label = read(&dir.join(format!("{stem}_label")).to_string_lossy())
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .map_or_else(|| format!("{chip} {stem}"), |l| format!("{chip} {l}"));
                    sensors.push((label, milli / 1000.0));
                }
            }
        }
        if sensors.is_empty() {
            if let Ok(zones) = std::fs::read_dir("/sys/class/thermal") {
                for zone in zones.flatten() {
                    let dir = zone.path();
                    if !zone
                        .file_name()
                        .to_string_lossy()
                        .starts_with("thermal_zone")
                    {
                        continue;
                    }
                    let Some(milli) = read(&dir.join("temp").to_string_lossy())
                        .and_then(|v| v.trim().parse::<f32>().ok())
                    else {
                        continue;
                    };
                    let label = read(&dir.join("type").to_string_lossy())
                        .map(|t| t.trim().to_string())
                        .unwrap_or_else(|| zone.file_name().to_string_lossy().into_owned());
                    sensors.push((label, milli / 1000.0));
                }
            }
        }
        sensors
    }

    pub(super) fn kernel_version() -> Option<String> {
        read("/proc/sys/kernel/osrelease").map(|v| v.trim().to_string())
    }

    pub(super) fn long_os_version() -> Option<String> {
        let text = read("/etc/os-release").or_else(|| read("/usr/lib/os-release"))?;
        text.lines().find_map(|line| {
            let value = line.strip_prefix("PRETTY_NAME=")?;
            Some(value.trim().trim_matches('"').to_string())
        })
    }

    pub(super) fn host_name() -> Option<String> {
        read("/proc/sys/kernel/hostname")
            .or_else(|| read("/etc/hostname"))
            .map(|h| h.trim().to_string())
    }

    /// `statvfs` on `mount`: `(total, available)` bytes.
    #[allow(unsafe_code)]
    fn statvfs_bytes(mount: &str) -> Option<(u64, u64)> {
        let path = std::ffi::CString::new(mount).ok()?;
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `path` is a NUL-terminated C string and `stats` is writable
        // storage of exactly one `statvfs`; both outlive the call.
        let rc = unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        // SAFETY: statvfs returned 0, so it initialized `stats`.
        let stats = unsafe { stats.assume_init() };
        let frsize = u64::from(stats.f_frsize);
        Some((
            u64::from(stats.f_blocks).saturating_mul(frsize),
            u64::from(stats.f_bavail).saturating_mul(frsize),
        ))
    }

    /// Octal escapes `/proc/mounts` uses for spaces and tabs in paths.
    fn unescape_mount(field: &str) -> String {
        field
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\")
    }

    pub(super) fn filesystems() -> Vec<Filesystem> {
        let Some(text) = read("/proc/mounts") else {
            return Vec::new();
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for line in text.lines() {
            let mut fields = line.split_whitespace();
            let (Some(device), Some(mount), Some(fstype)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if !device.starts_with("/dev/") || !seen.insert(mount.to_string()) {
                continue;
            }
            let mount = unescape_mount(mount);
            let Some((total_bytes, available_bytes)) = statvfs_bytes(&mount) else {
                continue;
            };
            if total_bytes == 0 {
                continue;
            }
            out.push(Filesystem {
                mount_point: PathBuf::from(mount),
                file_system: fstype.to_string(),
                total_bytes,
                available_bytes,
            });
        }
        out
    }
}

#[cfg(windows)]
mod platform {
    use super::{CpuTicks, Filesystem, MemorySnapshot};
    use winapi::host;

    pub(super) fn memory() -> Option<MemorySnapshot> {
        let status = host::memory_status()?;
        Some(MemorySnapshot {
            total_bytes: status.total_physical,
            available_bytes: status.available_physical.min(status.total_physical),
            total_swap_bytes: status.total_page_file.saturating_sub(status.total_physical),
            free_swap_bytes: status
                .available_page_file
                .saturating_sub(status.available_physical),
        })
    }

    pub(super) fn load_average() -> Option<[f64; 3]> {
        None
    }

    /// `GetSystemTimes`: kernel time includes idle time.
    pub(super) fn cpu_ticks() -> Option<CpuTicks> {
        let times = host::cpu_times()?;
        let total = times.kernel.saturating_add(times.user);
        Some(CpuTicks {
            busy: total.saturating_sub(times.idle),
            total,
        })
    }

    pub(super) fn uptime_secs() -> Option<u64> {
        Some(host::uptime_ms() / 1000)
    }

    fn registry_string(path: &str, name: &str) -> Option<String> {
        windows_registry::LOCAL_MACHINE
            .open(path)
            .ok()?
            .get_string(name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    const CURRENT_VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";

    pub(super) fn cpu_brand() -> Option<String> {
        registry_string(
            r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            "ProcessorNameString",
        )
    }

    pub(super) fn temperature_sensors() -> Vec<(String, f32)> {
        Vec::new()
    }

    fn build_number() -> Option<u64> {
        registry_string(CURRENT_VERSION, "CurrentBuildNumber")?
            .parse()
            .ok()
    }

    pub(super) fn kernel_version() -> Option<String> {
        let build = build_number()?;
        let revision = windows_registry::LOCAL_MACHINE
            .open(CURRENT_VERSION)
            .ok()
            .and_then(|key| key.get_u32("UBR").ok());
        Some(match revision {
            Some(ubr) => format!("{build}.{ubr}"),
            None => build.to_string(),
        })
    }

    pub(super) fn long_os_version() -> Option<String> {
        let mut name = registry_string(CURRENT_VERSION, "ProductName")?;
        // The registry still says "Windows 10" on Windows 11 hosts; the
        // build number tells them apart (22000 and up is Windows 11).
        if build_number().is_some_and(|b| b >= 22000) {
            name = name.replacen("Windows 10", "Windows 11", 1);
        }
        if let Some(release) = registry_string(CURRENT_VERSION, "DisplayVersion") {
            name.push(' ');
            name.push_str(&release);
        }
        Some(name)
    }

    pub(super) fn host_name() -> Option<String> {
        host::computer_name().or_else(|| std::env::var("COMPUTERNAME").ok())
    }

    pub(super) fn filesystems() -> Vec<Filesystem> {
        host::fixed_drives()
            .into_iter()
            .filter(|d| d.total_bytes > 0)
            .map(|d| Filesystem {
                mount_point: d.root,
                file_system: d.file_system,
                total_bytes: d.total_bytes,
                available_bytes: d.available_bytes,
            })
            .collect()
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use super::{CpuTicks, Filesystem, MemorySnapshot};

    pub(super) fn memory() -> Option<MemorySnapshot> {
        None
    }
    pub(super) fn load_average() -> Option<[f64; 3]> {
        None
    }
    pub(super) fn cpu_ticks() -> Option<CpuTicks> {
        None
    }
    pub(super) fn uptime_secs() -> Option<u64> {
        None
    }
    pub(super) fn cpu_brand() -> Option<String> {
        None
    }
    pub(super) fn temperature_sensors() -> Vec<(String, f32)> {
        Vec::new()
    }
    pub(super) fn kernel_version() -> Option<String> {
        None
    }
    pub(super) fn long_os_version() -> Option<String> {
        None
    }
    pub(super) fn host_name() -> Option<String> {
        None
    }
    pub(super) fn filesystems() -> Vec<Filesystem> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_reads_a_real_total_on_supported_hosts() {
        if !IS_SUPPORTED_SYSTEM {
            assert!(memory().is_none());
            return;
        }
        let mem = memory().unwrap();
        assert!(mem.total_bytes > 0);
        assert!(mem.available_bytes <= mem.total_bytes);
        assert!((0.0..=100.0).contains(&mem.used_pct()));
    }

    #[test]
    fn load_average_is_absent_exactly_where_the_os_has_none() {
        assert_eq!(
            load_average().is_none(),
            load_average_unavailable().is_some()
        );
        if cfg!(windows) {
            assert!(load_average().is_none());
        }
    }

    #[test]
    fn cpu_readings_stay_in_range() {
        if !IS_SUPPORTED_SYSTEM {
            assert!(cpu_util_pct().is_none());
            return;
        }
        for _ in 0..2 {
            let pct = cpu_util_pct().unwrap();
            assert!((0.0..=100.0).contains(&pct), "{pct}");
        }
        let pressure = cpu_pressure_pct().unwrap();
        assert!((0.0..=100.0).contains(&pressure), "{pressure}");
        assert!(cpu_count() >= 1);
    }

    #[test]
    fn host_identity_is_reported() {
        assert!(!kernel_name().is_empty());
        if IS_SUPPORTED_SYSTEM {
            assert!(uptime_secs().is_some_and(|s| s > 0));
            assert!(!host_name().is_empty());
            assert!(cpu_brand().is_some_and(|b| !b.is_empty()));
            assert!(kernel_version().is_some());
            assert!(long_os_version().is_some());
        }
    }

    #[test]
    fn kernel_version_and_long_os_version_never_return_whitespace_only_strings() {
        if let Some(kv) = kernel_version() {
            assert!(!kv.trim().is_empty());
        }
        if let Some(osv) = long_os_version() {
            assert!(!osv.trim().is_empty());
        }
    }

    #[test]
    fn the_current_directory_is_on_a_filesystem_with_space() {
        if !IS_SUPPORTED_SYSTEM {
            assert!(filesystems().is_empty());
            return;
        }
        let here = std::env::current_dir().unwrap();
        let fs = filesystem_of(&here).unwrap();
        assert!(fs.total_bytes > 0, "{fs:?}");
        assert!(fs.available_bytes <= fs.total_bytes, "{fs:?}");
        assert!(
            !fs.file_system.is_empty() || cfg!(target_os = "linux"),
            "{fs:?}"
        );
    }

    #[test]
    fn sensors_have_labels_and_finite_positive_readings() {
        for (label, celsius) in temperature_sensors() {
            assert!(!label.trim().is_empty());
            assert!(celsius.is_finite() && celsius > 0.0, "{label}: {celsius}");
        }
    }
}
