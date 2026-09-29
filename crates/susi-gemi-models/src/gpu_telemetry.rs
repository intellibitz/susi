//! GPU telemetry sampler: parse nvidia-smi / rocm-smi style tool output into
//! a bounded time series for the hardware profile.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// One GPU sample from a telemetry tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuSample {
    pub util_pct: f32,
    pub vram_used_mb: u32,
    pub vram_total_mb: u32,
    pub temp_c: f32,
    pub power_w: f32,
    pub unix_secs: u64,
}

/// Bounded ring of recent samples.
#[derive(Debug, Clone)]
pub struct GpuTelemetrySeries {
    capacity: usize,
    samples: VecDeque<GpuSample>,
}

impl GpuTelemetrySeries {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            samples: VecDeque::new(),
        }
    }

    pub fn push(&mut self, sample: GpuSample) {
        if self.samples.len() >= self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    #[must_use]
    pub fn latest(&self) -> Option<&GpuSample> {
        self.samples.back()
    }

    pub fn samples(&self) -> impl Iterator<Item = &GpuSample> {
        self.samples.iter()
    }
}

/// Parse a recorded `nvidia-smi --query-gpu=... --format=csv,noheader,nounits`
/// line: `util, vram_used, vram_total, temp, power`.
#[must_use]
pub fn parse_nvidia_smi_csv_line(line: &str, unix_secs: u64) -> Option<GpuSample> {
    let parts: Vec<&str> = line.split(',').map(str::trim).collect();
    if parts.len() < 5 {
        return None;
    }
    Some(GpuSample {
        util_pct: parts[0].parse().ok()?,
        vram_used_mb: parts[1].parse().ok()?,
        vram_total_mb: parts[2].parse().ok()?,
        temp_c: parts[3].parse().ok()?,
        power_w: parts[4].parse().ok()?,
        unix_secs,
    })
}

/// Parse a recorded `rocm-smi` style line:
/// `GPU[0] : Temperature (Sensor edge) (C): 62` plus companion util/vram/power
/// key=value tokens on one line.
#[must_use]
pub fn parse_rocm_smi_kv_line(line: &str, unix_secs: u64) -> Option<GpuSample> {
    let mut util = None;
    let mut vram_used = None;
    let mut vram_total = None;
    let mut temp = None;
    let mut power = None;
    for tok in line.split_whitespace() {
        if let Some(v) = tok.strip_prefix("util=") {
            util = v.trim_end_matches('%').parse().ok();
        } else if let Some(v) = tok.strip_prefix("vram_used=") {
            vram_used = v.trim_end_matches("MB").parse().ok();
        } else if let Some(v) = tok.strip_prefix("vram_total=") {
            vram_total = v.trim_end_matches("MB").parse().ok();
        } else if let Some(v) = tok.strip_prefix("temp=") {
            temp = v.trim_end_matches('C').parse().ok();
        } else if let Some(v) = tok.strip_prefix("power=") {
            power = v.trim_end_matches('W').parse().ok();
        }
    }
    Some(GpuSample {
        util_pct: util?,
        vram_used_mb: vram_used?,
        vram_total_mb: vram_total?,
        temp_c: temp?,
        power_w: power?,
        unix_secs,
    })
}

#[cfg(test)]
mod gpu_telemetry_tests {
    use super::*;

    #[test]
    fn gpu_telemetry_parses_nvidia_smi_csv() {
        let s = parse_nvidia_smi_csv_line("45, 2048, 8192, 71, 120.5", 1_700_000_000).unwrap();
        assert_eq!(s.util_pct, 45.0);
        assert_eq!(s.vram_used_mb, 2048);
        assert_eq!(s.vram_total_mb, 8192);
        assert_eq!(s.temp_c, 71.0);
        assert_eq!(s.power_w, 120.5);
    }

    #[test]
    fn gpu_telemetry_parses_rocm_smi_kv() {
        let s = parse_rocm_smi_kv_line(
            "GPU[0] util=55% vram_used=1024MB vram_total=16384MB temp=62C power=90W",
            42,
        )
        .unwrap();
        assert_eq!(s.util_pct, 55.0);
        assert_eq!(s.temp_c, 62.0);
        assert_eq!(s.power_w, 90.0);
    }

    #[test]
    fn gpu_telemetry_series_is_bounded() {
        let mut series = GpuTelemetrySeries::new(2);
        series.push(parse_nvidia_smi_csv_line("10,1,2,30,40", 1).unwrap());
        series.push(parse_nvidia_smi_csv_line("20,1,2,30,40", 2).unwrap());
        series.push(parse_nvidia_smi_csv_line("30,1,2,30,40", 3).unwrap());
        assert_eq!(series.len(), 2);
        assert_eq!(series.latest().unwrap().util_pct, 30.0);
        assert_eq!(series.samples().next().unwrap().unix_secs, 2);
    }
}
