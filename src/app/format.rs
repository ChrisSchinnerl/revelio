//! Small formatting helpers for sizes, times, durations, and prices.

use std::time::Duration;

pub(crate) fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Trims an RFC3339 timestamp to `YYYY-MM-DD HH:MM:SS` for display.
pub(crate) fn short_time(ts: &str) -> String {
    ts.get(..19).unwrap_or(ts).replace('T', " ")
}

/// Formats an elapsed duration as a compact "… ago" string.
pub(crate) fn ago(d: Duration) -> String {
    let s = d.as_secs();
    if s < 5 {
        "just now".to_string()
    } else if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else {
        format!("{}h ago", s / 3600)
    }
}

/// Formats a duration in seconds: sub-minute as `3.2s`, longer as `1m 05s`.
pub(crate) fn fmt_duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let total = secs.round() as u64;
        format!("{}m {:02}s", total / 60, total % 60)
    }
}

/// Formats a USD amount, using more decimals for sub-dollar values.
pub(crate) fn usd(v: f64) -> String {
    if v != 0.0 && v.abs() < 1.0 {
        format!("${v:.4}")
    } else {
        format!("${v:.2}")
    }
}

/// The indexer uses a near-`i64::MAX` sentinel for "no limit".
pub(crate) fn is_limited(bytes: u64) -> bool {
    bytes < (1u64 << 60)
}

/// Like [`human_size`] but renders the "no limit" sentinel as "unlimited".
pub(crate) fn max_size(bytes: u64) -> String {
    if is_limited(bytes) {
        human_size(bytes)
    } else {
        "unlimited".to_string()
    }
}
