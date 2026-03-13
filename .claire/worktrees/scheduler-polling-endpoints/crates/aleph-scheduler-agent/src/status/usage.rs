//! Host resource usage collection from /proc and statvfs.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

// Types and implementation will go here after tests.

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
