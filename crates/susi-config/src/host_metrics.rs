//! Host-wide CPU, memory, load and uptime readings on every platform the
//! core ships on, through the vendored `sysinfo` facade.
//!
//! These readers used to be scattered `/proc` parsers that answered zero or
//! nothing on Windows and macOS. `sysinfo` reads the same kernel sources on
//! Linux and the native APIs elsewhere. A metric an OS does not have (a load
//! average on Windows) answers `None`, and [`load_average_unavailable`] names
//! why, so callers report "unavailable" instead of printing a made-up zero.

use parking_lot::Mutex;
use std::sync::OnceLock;
use std::time::Instant;
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

/// Physical and swap memory, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySnapshot {
    /// Installed physical memory.
    pub total_bytes: u64,
    /// Memory available to new work without swapping (Linux `MemAvailable`,
    /// the Windows "available" figure, macOS total minus used).
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
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return None;
    }
    let mut system = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram().with_swap()),
    );
    system.refresh_memory();
    let total_bytes = system.total_memory();
    if total_bytes == 0 {
        return None;
    }
    // macOS: sysinfo's `available_memory` counts active pages too, which
    // would understate pressure; total minus used (app + wired +
    // compressed, as Activity Monitor reports it) is the honest figure.
    let available_bytes = if cfg!(target_os = "macos") {
        total_bytes.saturating_sub(system.used_memory())
    } else {
        system.available_memory().min(total_bytes)
    };
    Some(MemorySnapshot {
        total_bytes,
        available_bytes,
        total_swap_bytes: system.total_swap(),
        free_swap_bytes: system.free_swap(),
    })
}

/// The 1, 5 and 15 minute load averages. `None` on Windows, which keeps no
/// load average (see [`cpu_util_pct`] there), and on unsupported targets.
pub fn load_average() -> Option<[f64; 3]> {
    if load_average_unavailable().is_some() {
        return None;
    }
    let load = System::load_average();
    Some([load.one, load.five, load.fifteen])
}

/// Why [`load_average`] answers `None` on this OS, or `None` when it reads.
pub fn load_average_unavailable() -> Option<&'static str> {
    if cfg!(windows) {
        Some("Windows keeps no load average; use the sampled CPU utilization instead")
    } else if !sysinfo::IS_SUPPORTED_SYSTEM {
        Some("this OS has no supported load-average source")
    } else {
        None
    }
}

/// One process-wide CPU sampler. CPU utilization is a delta between two
/// readings, so the sampler keeps its last reading: a long-lived caller (the
/// daemon's governor) gets the utilization since its previous call with no
/// wait, and only the very first call in a process waits one minimum
/// sampling interval.
struct CpuSampler {
    system: System,
    taken: Instant,
    primed: bool,
    last_pct: f32,
}

fn cpu_sampler() -> &'static Mutex<CpuSampler> {
    static SAMPLER: OnceLock<Mutex<CpuSampler>> = OnceLock::new();
    SAMPLER.get_or_init(|| {
        let mut system = System::new_with_specifics(
            RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_cpu_usage()),
        );
        system.refresh_cpu_usage();
        Mutex::new(CpuSampler {
            system,
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
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return None;
    }
    let mut sampler = cpu_sampler().lock();
    let elapsed = sampler.taken.elapsed();
    if elapsed < sysinfo::MINIMUM_CPU_UPDATE_INTERVAL {
        if sampler.primed {
            // Too soon for a new delta: the last one is still the truth.
            return Some(sampler.last_pct);
        }
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL - elapsed);
    }
    sampler.system.refresh_cpu_usage();
    sampler.taken = Instant::now();
    sampler.primed = true;
    let pct = sampler.system.global_cpu_usage();
    sampler.last_pct = if pct.is_finite() {
        pct.clamp(0.0, 100.0)
    } else {
        0.0
    };
    Some(sampler.last_pct)
}

/// CPU pressure in percent (0-100) for governors and the `top` view: the
/// 1-minute load average per logical CPU where the OS keeps one (Linux,
/// macOS), the sampled utilization on Windows. `None` when neither exists.
pub fn cpu_pressure_pct() -> Option<f32> {
    match load_average() {
        Some([one, _, _]) => Some(((one / cpu_count() as f64) * 100.0).clamp(0.0, 100.0) as f32),
        None => cpu_util_pct(),
    }
}

/// Seconds since boot, or `None` when this OS reports none.
pub fn uptime_secs() -> Option<u64> {
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return None;
    }
    Some(System::uptime()).filter(|secs| *secs > 0)
}

/// The CPU model string ("AMD Ryzen 9 7950X 16-Core Processor"), when the
/// OS names one.
pub fn cpu_brand() -> Option<String> {
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return None;
    }
    let system =
        System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing()));
    system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().trim().to_string())
        .filter(|brand| !brand.is_empty())
}

/// Temperature sensors the OS exposes, as `(label, degrees Celsius)`.
/// macOS reads its SMC sensors; Windows asks WMI, which on many hosts
/// exposes none to an unelevated process. Sensors without a reading are
/// skipped, so an empty list means "none readable here".
pub fn temperature_sensors() -> Vec<(String, f32)> {
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return Vec::new();
    }
    sysinfo::Components::new_with_refreshed_list()
        .list()
        .iter()
        .filter_map(|c| {
            let celsius = c.temperature().filter(|t| t.is_finite() && *t > 0.0)?;
            Some((c.label().to_string(), celsius))
        })
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

/// The kernel release ("6.8.0-45-generic", "24.1.0", the Windows build
/// number), when the OS reports it.
pub fn kernel_version() -> Option<String> {
    System::kernel_version().filter(|v| !v.trim().is_empty())
}

/// The human OS name and version ("Ubuntu 24.04 LTS", "Windows 11 Pro"),
/// when the OS reports it.
pub fn long_os_version() -> Option<String> {
    System::long_os_version().filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_reads_a_real_total_on_supported_hosts() {
        if !sysinfo::IS_SUPPORTED_SYSTEM {
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
        if !sysinfo::IS_SUPPORTED_SYSTEM {
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
        if sysinfo::IS_SUPPORTED_SYSTEM {
            assert!(uptime_secs().is_some_and(|s| s > 0));
        }
    }
}
