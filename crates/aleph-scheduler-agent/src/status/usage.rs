//! Host resource usage collection from /proc and statvfs.
//!
//! Matches the response schema of aleph-vm's `GET /about/usage/system`.

use anyhow::{Context, Result};
use memsizes::{Bytes, KB, KiB, MemorySize, Rounding};
use serde::Serialize;
use std::path::Path;
use std::time::SystemTime;

#[derive(Debug, Serialize)]
pub struct MachineUsage {
    pub cpu: CpuUsage,
    pub mem: MemoryUsage,
    pub disk: DiskUsage,
    pub period: UsagePeriod,
    pub properties: MachineProperties,
    #[serde(skip_serializing_if = "GpuProperties::is_empty")]
    pub gpu: GpuProperties,
    pub active: bool,
}

#[derive(Debug, Serialize)]
pub struct CpuUsage {
    pub count: u32,
    pub load_average: LoadAverage,
    pub core_frequencies: CoreFrequencies,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadAverage {
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoreFrequencies {
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Serialize)]
pub struct MemoryUsage {
    #[serde(rename = "total_kB")]
    pub total_kb: KB,
    #[serde(rename = "available_kB")]
    pub available_kb: KB,
}

#[derive(Debug, Serialize)]
pub struct DiskUsage {
    #[serde(rename = "total_kB")]
    pub total_kb: KB,
    #[serde(rename = "available_kB")]
    pub available_kb: KB,
}

#[derive(Debug, Serialize)]
pub struct UsagePeriod {
    pub start_timestamp: String,
    pub duration_seconds: f64,
}

#[derive(Debug, Serialize)]
pub struct MachineProperties {
    pub cpu: CpuProperties,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuProperties {
    pub architecture: String,
    pub vendor: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct GpuProperties {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_devices: Option<Vec<serde_json::Value>>,
}

impl GpuProperties {
    fn is_empty(&self) -> bool {
        self.devices.is_none() && self.available_devices.is_none()
    }
}

/// Count processors from /proc/cpuinfo content.
pub fn parse_cpu_count(cpuinfo: &str) -> u32 {
    cpuinfo
        .lines()
        .filter(|line| line.starts_with("processor"))
        .count() as u32
}

/// Parse the vendor_id from /proc/cpuinfo (first occurrence).
pub fn parse_cpu_vendor(cpuinfo: &str) -> String {
    for line in cpuinfo.lines() {
        if let Some(rest) = line.strip_prefix("vendor_id")
            && let Some(val) = rest.split(':').nth(1)
        {
            return val.trim().to_string();
        }
    }
    String::new()
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
///
/// /proc/meminfo reports values in KiB (binary). We convert to decimal KB
/// (via bytes) to match aleph-vm's `psutil.virtual_memory().total / 1000`.
pub fn parse_meminfo(meminfo: &str) -> Result<MemoryUsage> {
    let mut total = None;
    let mut available = None;
    for line in meminfo.lines() {
        if let Some(val) = line.strip_prefix("MemTotal:") {
            total = Some(parse_kib_as_kb(val, Rounding::Ceil)?);
        } else if let Some(val) = line.strip_prefix("MemAvailable:") {
            available = Some(parse_kib_as_kb(val, Rounding::Floor)?);
        }
    }
    Ok(MemoryUsage {
        total_kb: total.context("MemTotal not found in /proc/meminfo")?,
        available_kb: available.context("MemAvailable not found in /proc/meminfo")?,
    })
}

/// Parse a KiB integer from /proc/meminfo and convert to decimal KB.
fn parse_kib_as_kb(val: &str, rounding: Rounding) -> Result<KB> {
    let kib_value: u64 = val
        .split_whitespace()
        .next()
        .context("empty value")?
        .parse()
        .context("invalid integer")?;
    let kib = KiB::from_units(kib_value);
    let bytes = Bytes::from(kib);
    bytes
        .to_rounded::<KB>(rounding)
        .context("KiB to KB conversion overflow")
}

/// Parse CPU frequency from sysfs (MHz). Returns (min, max).
///
/// Reads from `/sys/devices/system/cpu/cpu0/cpufreq/`. Falls back to
/// /proc/cpuinfo "cpu MHz" if sysfs is unavailable.
pub async fn parse_cpu_frequencies() -> CoreFrequencies {
    // Try sysfs first (values in kHz)
    let min = tokio::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_min_freq")
        .await
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|khz| khz / 1000.0);

    let max = tokio::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq")
        .await
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|khz| khz / 1000.0);

    // If both are available, use them
    if let (Some(min_mhz), Some(max_mhz)) = (min, max) {
        return CoreFrequencies {
            min: min_mhz,
            max: max_mhz,
        };
    }

    // Fallback: read current freq from /proc/cpuinfo
    let current = tokio::fs::read_to_string("/proc/cpuinfo")
        .await
        .ok()
        .and_then(|cpuinfo| {
            cpuinfo.lines().find_map(|line| {
                line.strip_prefix("cpu MHz")
                    .and_then(|rest| rest.split(':').nth(1))
                    .and_then(|val| val.trim().parse::<f64>().ok())
            })
        })
        .unwrap_or(0.0);

    CoreFrequencies {
        min: min.unwrap_or(current),
        max: max.unwrap_or(current),
    }
}

/// Detect AMD SEV features by checking sysfs.
pub async fn detect_sev_features() -> Vec<String> {
    let mut features = Vec::new();
    // Check /sys/module/kvm_amd/parameters/sev
    if is_sysfs_flag_enabled("/sys/module/kvm_amd/parameters/sev").await {
        features.push("sev".to_string());
    }
    if is_sysfs_flag_enabled("/sys/module/kvm_amd/parameters/sev_es").await {
        features.push("sev_es".to_string());
    }
    if is_sysfs_flag_enabled("/sys/module/kvm_amd/parameters/sev_snp").await {
        features.push("sev_snp".to_string());
    }
    features
}

async fn is_sysfs_flag_enabled(path: &str) -> bool {
    tokio::fs::read_to_string(path)
        .await
        .ok()
        .map(|s| matches!(s.trim(), "1" | "Y" | "y"))
        .unwrap_or(false)
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
    let frsize = stat.f_frsize as u64;
    let total_bytes = Bytes::from_units(stat.f_blocks * frsize);
    let avail_bytes = Bytes::from_units(stat.f_bavail * frsize);
    Ok(DiskUsage {
        total_kb: total_bytes
            .to_rounded::<KB>(Rounding::Floor)
            .context("disk total overflow")?,
        available_kb: avail_bytes
            .to_rounded::<KB>(Rounding::Floor)
            .context("disk available overflow")?,
    })
}

/// Format a SystemTime as ISO 8601 UTC string, truncated to the current minute.
fn format_period_timestamp(t: SystemTime) -> String {
    let dur = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let total_secs = dur.as_secs();
    // Truncate to start of the current minute
    let truncated = total_secs - (total_secs % 60);

    // Manual UTC formatting (avoids chrono dependency)
    let (year, month, day, hour, min) = unix_to_utc(truncated);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:00+00:00")
}

/// Convert a unix timestamp to (year, month, day, hour, minute) in UTC.
fn unix_to_utc(secs: u64) -> (u64, u64, u64, u64, u64) {
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hour = time_of_day / 3600;
    let min = (time_of_day % 3600) / 60;

    // Days since 1970-01-01 to (year, month, day) — civil calendar algorithm
    // From Howard Hinnant's date algorithms
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    (y, m, d, hour, min)
}

/// Collect all system usage metrics.
pub async fn collect_usage(disk_path: &Path) -> Result<MachineUsage> {
    let cpuinfo = tokio::fs::read_to_string("/proc/cpuinfo").await?;
    let loadavg = tokio::fs::read_to_string("/proc/loadavg").await?;
    let meminfo = tokio::fs::read_to_string("/proc/meminfo").await?;

    let core_frequencies = parse_cpu_frequencies().await;
    let sev_features = detect_sev_features().await;
    let vendor = parse_cpu_vendor(&cpuinfo);

    // Run blocking statvfs off the async runtime
    let disk_path_owned = disk_path.to_path_buf();
    let disk = tokio::task::spawn_blocking(move || disk_usage(&disk_path_owned))
        .await
        .context("disk_usage task panicked")??;

    Ok(MachineUsage {
        cpu: CpuUsage {
            count: parse_cpu_count(&cpuinfo),
            load_average: parse_load_average(&loadavg)?,
            core_frequencies,
        },
        mem: parse_meminfo(&meminfo)?,
        disk,
        period: UsagePeriod {
            start_timestamp: format_period_timestamp(SystemTime::now()),
            duration_seconds: 60.0,
        },
        properties: MachineProperties {
            cpu: CpuProperties {
                architecture: std::env::consts::ARCH.to_string(),
                vendor,
                features: sev_features,
            },
        },
        gpu: GpuProperties {
            devices: None,
            available_devices: None,
        },
        active: true,
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
    fn test_parse_cpu_vendor() {
        let cpuinfo = "\
processor\t: 0
vendor_id\t: AuthenticAMD
model name\t: AMD EPYC
";
        assert_eq!(parse_cpu_vendor(cpuinfo), "AuthenticAMD");
    }

    #[test]
    fn test_parse_cpu_vendor_missing() {
        assert_eq!(parse_cpu_vendor("processor\t: 0\n"), "");
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
        // /proc/meminfo reports KiB; converted to decimal KB (× 1024 / 1000)
        // 16384000 KiB = 16384000 * 1024 bytes = 16777216000 bytes
        // ceil(16777216000 / 1000) = 16777216 KB (divides evenly)
        assert_eq!(result.total_kb, KB::from_units(16777216));
        // 8192000 KiB = 8192000 * 1024 bytes = 8388608000 bytes
        // floor(8388608000 / 1000) = 8388608 KB (Floor for available)
        assert_eq!(result.available_kb, KB::from_units(8388608));
    }

    #[test]
    fn test_parse_meminfo_missing_available() {
        let meminfo = "MemTotal:       16384000 kB\n";
        assert!(parse_meminfo(meminfo).is_err());
    }

    #[test]
    fn test_disk_usage_on_tmp() {
        let usage = disk_usage(Path::new("/tmp")).unwrap();
        assert!(usage.total_kb.units() > 0);
        assert!(usage.available_kb.units() > 0);
        assert!(usage.total_kb >= usage.available_kb);
    }

    #[test]
    fn test_format_period_timestamp() {
        // 2024-01-15 11:34:56 UTC → truncate to 11:34:00
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1705318496);
        let ts = format_period_timestamp(t);
        assert_eq!(ts, "2024-01-15T11:34:00+00:00");
    }

    #[test]
    fn test_unix_to_utc_epoch() {
        let (y, m, d, h, min) = unix_to_utc(0);
        assert_eq!((y, m, d, h, min), (1970, 1, 1, 0, 0));
    }

    #[test]
    fn test_unix_to_utc_known_date() {
        // 2024-03-15 09:30:00 UTC = 1710495000
        let (y, m, d, h, min) = unix_to_utc(1710495000);
        assert_eq!((y, m, d, h, min), (2024, 3, 15, 9, 30));
    }

    #[test]
    fn test_gpu_properties_is_empty() {
        let empty = GpuProperties {
            devices: None,
            available_devices: None,
        };
        assert!(empty.is_empty());

        let non_empty = GpuProperties {
            devices: Some(vec![]),
            available_devices: None,
        };
        assert!(!non_empty.is_empty());
    }

    #[test]
    fn test_machine_usage_json_structure() {
        let usage = MachineUsage {
            cpu: CpuUsage {
                count: 4,
                load_average: LoadAverage {
                    load1: 0.1,
                    load5: 0.2,
                    load15: 0.3,
                },
                core_frequencies: CoreFrequencies {
                    min: 800.0,
                    max: 3600.0,
                },
            },
            mem: MemoryUsage {
                total_kb: KB::from_units(16000000),
                available_kb: KB::from_units(8000000),
            },
            disk: DiskUsage {
                total_kb: KB::from_units(500000000),
                available_kb: KB::from_units(250000000),
            },
            period: UsagePeriod {
                start_timestamp: "2024-01-15T12:00:00+00:00".to_string(),
                duration_seconds: 60.0,
            },
            properties: MachineProperties {
                cpu: CpuProperties {
                    architecture: "x86_64".to_string(),
                    vendor: "AuthenticAMD".to_string(),
                    features: vec!["sev".to_string(), "sev_snp".to_string()],
                },
            },
            gpu: GpuProperties {
                devices: None,
                available_devices: None,
            },
            active: true,
        };
        let json = serde_json::to_value(&usage).unwrap();
        assert_eq!(json["cpu"]["count"], 4);
        assert_eq!(json["cpu"]["core_frequencies"]["min"], 800.0);
        assert_eq!(json["period"]["duration_seconds"], 60.0);
        assert_eq!(json["properties"]["cpu"]["architecture"], "x86_64");
        assert_eq!(json["active"], true);
        // gpu should be omitted when empty
        assert!(json.get("gpu").is_none());
    }
}
