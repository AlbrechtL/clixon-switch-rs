//! System state (`/system/state` of clixon-switch): host name, OS release,
//! uptime, load and memory.
//!
//! The parsers take the contents of the /proc and /etc files, so they run
//! on the host; the plugin reads the files. A file that is missing or does
//! not parse leaves its leaves out.

use std::fmt::Write;

use crate::{escape, SWITCH_NS};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct SystemState {
    pub hostname: Option<String>,
    /// NAME of os-release.
    pub os_name: Option<String>,
    /// VERSION of os-release.
    pub os_version: Option<String>,
    /// /proc/sys/kernel/osrelease.
    pub kernel_release: Option<String>,
    /// Seconds since boot.
    pub uptime: Option<u64>,
    /// 1, 5 and 15 minute load averages as printed by the kernel (two
    /// fraction digits).
    pub load_average: Option<[String; 3]>,
    /// kB.
    pub memory_total: Option<u64>,
    /// kB.
    pub memory_available: Option<u64>,
    /// Seconds since the Unix epoch.
    pub current_time: Option<u64>,
}

/// NAME and VERSION of an os-release file.
pub fn parse_os_release(text: &str) -> (Option<String>, Option<String>) {
    let mut name = None;
    let mut version = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value)
            .to_string();
        match key.trim() {
            "NAME" => name = Some(value),
            "VERSION" => version = Some(value),
            _ => {}
        }
    }
    (name, version)
}

/// Whole seconds since boot from /proc/uptime.
pub fn parse_uptime(text: &str) -> Option<u64> {
    let seconds = text.split_whitespace().next()?;
    seconds.split('.').next()?.parse().ok()
}

/// The three load averages from /proc/loadavg.
pub fn parse_loadavg(text: &str) -> Option<[String; 3]> {
    let mut fields = text.split_whitespace();
    let mut next = || {
        let value = fields.next()?;
        value.parse::<f64>().ok()?;
        Some(value.to_string())
    };
    Some([next()?, next()?, next()?])
}

/// MemTotal and MemAvailable (kB) from /proc/meminfo.
pub fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let kb = value.split_whitespace().next().and_then(|v| v.parse().ok());
        match key {
            "MemTotal" => total = kb,
            "MemAvailable" => available = kb,
            _ => {}
        }
    }
    (total, available)
}

/// RFC 3339 date and time in UTC.
pub fn date_and_time(epoch_seconds: u64) -> String {
    let days = (epoch_seconds / 86400) as i64;
    let secs = epoch_seconds % 86400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    )
}

/// State data XML of `/system`, in the YANG order.
pub fn system_state_xml(system: &SystemState) -> String {
    fn leaf(xml: &mut String, name: &str, value: Option<impl std::fmt::Display>) {
        if let Some(value) = value {
            let _ = write!(xml, "<{name}>{value}</{name}>");
        }
    }
    let text = |value: &Option<String>| value.as_deref().map(escape);
    let mut xml = format!(r#"<system xmlns="{SWITCH_NS}"><state>"#);
    leaf(&mut xml, "hostname", text(&system.hostname));
    leaf(&mut xml, "os-name", text(&system.os_name));
    leaf(&mut xml, "os-version", text(&system.os_version));
    leaf(&mut xml, "kernel-release", text(&system.kernel_release));
    leaf(
        &mut xml,
        "current-datetime",
        system.current_time.map(date_and_time),
    );
    leaf(&mut xml, "uptime", system.uptime);
    let load = system.load_average.as_ref();
    leaf(&mut xml, "load-average-1", load.map(|l| &l[0]));
    leaf(&mut xml, "load-average-5", load.map(|l| &l[1]));
    leaf(&mut xml, "load-average-15", load.map(|l| &l[2]));
    leaf(&mut xml, "memory-total", system.memory_total);
    leaf(&mut xml, "memory-available", system.memory_available);
    xml.push_str("</state></system>");
    xml
}
