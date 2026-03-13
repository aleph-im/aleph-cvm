//! Host resource usage collection from /proc and statvfs.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
pub struct MachineUsage {
    pub cpu: CpuUsage,
    pub mem: MemoryUsage,
    pub disk: DiskUsage,
}

#[derive(Debug, Serialize)]
pub struct CpuUsage {
    pub count: u32,
    pub load_average: LoadAverage,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadAverage {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

#[derive(Debug, Serialize)]
pub struct MemoryUsage {
    #[serde(rename = "total_kB")]
    pub total_kb: u64,
    #[serde(rename = "available_kB")]
    pub available_kb: u64,
}

#[derive(Debug, Serialize)]
pub struct DiskUsage {
    #[serde(rename = "total_kB")]
    pub total_kb: u64,
    #[serde(rename = "available_kB")]
    pub available_kb: u64,
}

/// Count processors from /proc/cpuinfo content.
pub fn parse_cpu_count(cpuinfo: &str) -> u32 {
    cpuinfo
        .lines()
        .filter(|line| line.starts_with("processor"))
        .count() as u32
}

/// Parse load averages from /proc/loadavg content.
pub fn parse_load_average(loadavg: &str) -> Result<LoadAverage> {
    let parts: Vec<&str> = loadavg.split_whitespace().collect();
    anyhow::ensure!(parts.len() >= 3, "invalid /proc/loadavg format");
    Ok(LoadAverage {
        load1: parts[0].parse().context("load1")?,
        load5: parts[1].parse().context("load5")?,
        load15: parts[2].parse().context("load15")?,
    })
}

/// Parse MemTotal and MemAvailable from /proc/meminfo content.
pub fn parse_meminfo(meminfo: &str) -> Result<MemoryUsage> {
    let mut total = None;
    let mut available = None;
    for line in meminfo.lines() {
        if let Some(val) = line.strip_prefix("MemTotal:") {
            total = Some(parse_kb_value(val)?);
        } else if let Some(val) = line.strip_prefix("MemAvailable:") {
            available = Some(parse_kb_value(val)?);
        }
    }
    Ok(MemoryUsage {
        total_kb: total.context("MemTotal not found in /proc/meminfo")?,
        available_kb: available.context("MemAvailable not found in /proc/meminfo")?,
    })
}

fn parse_kb_value(val: &str) -> Result<u64> {
    val.split_whitespace()
        .next()
        .context("empty value")?
        .parse()
        .context("invalid integer")
}

/// Get filesystem usage via statvfs.
pub fn disk_usage(path: &Path) -> Result<DiskUsage> {
    let c_path = std::ffi::CString::new(path.to_str().context("non-UTF8 path")?)?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if ret != 0 {
        anyhow::bail!(
            "statvfs({}): {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(DiskUsage {
        total_kb: stat.f_blocks * stat.f_frsize / 1024,
        available_kb: stat.f_bavail * stat.f_frsize / 1024,
    })
}

/// Collect all system usage metrics.
pub async fn collect_usage(disk_path: &Path) -> Result<MachineUsage> {
    let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await?;
    let loadavg = tokio::fs::read_to_string("/proc/loadavg").await?;
    let meminfo = tokio::fs::read_to_string("/proc/meminfo").await?;

    Ok(MachineUsage {
        cpu: CpuUsage {
            count: parse_cpu_count(&cpuinfo),
            load_average: parse_load_average(&loadavg)?,
        },
        mem: parse_meminfo(&meminfo)?,
        disk: disk_usage(disk_path)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_parse_cpu_count() {
        let cpuinfo = "\
processor\t: 0
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7

processor\t: 1
vendor_id\t: GenuineIntel
model name\t: Intel(R) Core(TM) i7

processor\t: 2
vendor_id\t: GenuineIntel

processor\t: 3
vendor_id\t: GenuineIntel
";
        assert_eq!(parse_cpu_count(cpuinfo), 4);
    }

    #[test]
    fn test_parse_cpu_count_single() {
        let cpuinfo = "processor\t: 0\nvendor_id\t: GenuineIntel\n";
        assert_eq!(parse_cpu_count(cpuinfo), 1);
    }

    #[test]
    fn test_parse_load_average() {
        let loadavg = "0.08 0.03 0.01 1/234 12345\n";
        let result = parse_load_average(loadavg).unwrap();
        assert!((result.load1 - 0.08).abs() < f64::EPSILON);
        assert!((result.load5 - 0.03).abs() < f64::EPSILON);
        assert!((result.load15 - 0.01).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_load_average_high_load() {
        let loadavg = "12.50 8.25 4.00 5/500 99999\n";
        let result = parse_load_average(loadavg).unwrap();
        assert!((result.load1 - 12.50).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_meminfo() {
        let meminfo = "\
MemTotal:       16384000 kB
MemFree:         1234000 kB
MemAvailable:    8192000 kB
Buffers:          456000 kB
";
        let result = parse_meminfo(meminfo).unwrap();
        assert_eq!(result.total_kb, 16384000);
        assert_eq!(result.available_kb, 8192000);
    }

    #[test]
    fn test_parse_meminfo_missing_available() {
        let meminfo = "MemTotal:       16384000 kB\n";
        assert!(parse_meminfo(meminfo).is_err());
    }

    #[test]
    fn test_disk_usage_on_tmp() {
        let usage = disk_usage(Path::new("/tmp")).unwrap();
        assert!(usage.total_kb > 0);
        assert!(usage.available_kb > 0);
        assert!(usage.total_kb >= usage.available_kb);
    }
}
