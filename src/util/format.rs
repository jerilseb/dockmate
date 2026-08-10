//! Human-friendly formatting helpers shared by every view.

use unicode_width::UnicodeWidthChar;

/// Format a byte count the way the Docker CLI does: 3 significant-ish digits
/// with a binary-ish suffix (`1.2GB`, `812MB`, `4.0kB`).
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "kB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n}B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0}{}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1}{}", UNITS[unit])
    } else {
        format!("{value:.2}{}", UNITS[unit])
    }
}

/// Same as [`bytes`] but tolerant of the signed counts Docker hands back.
pub fn bytes_i64(n: i64) -> String {
    if n <= 0 { "-".into() } else { bytes(n as u64) }
}

/// A short, chatty duration: `3s`, `12m`, `4h`, `9d`, `2mo`, `3y`.
pub fn duration_short(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        86_400..=2_591_999 => format!("{}d", s / 86_400),
        2_592_000..=31_535_999 => format!("{}mo", s / 2_592_000),
        _ => format!("{}y", s / 31_536_000),
    }
}

/// `"4 days ago"`-style text for a unix timestamp, used in detail panes.
pub fn age_from_epoch(epoch_secs: i64) -> String {
    if epoch_secs <= 0 {
        return "-".into();
    }
    let now = chrono::Utc::now().timestamp();
    format!("{} ago", duration_short(now - epoch_secs))
}

/// The 12-character prefix Docker shows everywhere. Strips a `sha256:` prefix.
pub fn short_id(id: &str) -> String {
    let id = id.strip_prefix("sha256:").unwrap_or(id);
    id.chars().take(12).collect()
}

/// Display width of a string in terminal cells.
pub fn width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Truncate to `max` display columns, appending `…` when anything was cut.
/// Operates on display width rather than bytes so CJK and emoji don't overflow
/// their column.
pub fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if width(s) <= max {
        return s.to_string();
    }
    let budget = max.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Truncate from the left instead, keeping the tail (useful for long paths and
/// registry-qualified image names where the interesting part is at the end).
pub fn truncate_start(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if width(s) <= max {
        return s.to_string();
    }
    let budget = max.saturating_sub(1);
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0;
    for c in s.chars().rev() {
        let w = c.width().unwrap_or(0);
        if used + w > budget {
            break;
        }
        tail.push(c);
        used += w;
    }
    let mut out = String::from("…");
    out.extend(tail.into_iter().rev());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_scales() {
        assert_eq!(bytes(512), "512B");
        assert_eq!(bytes(1024), "1.00kB");
        assert_eq!(bytes(1024 * 1024 * 5), "5.00MB");
        assert_eq!(bytes(1024 * 1024 * 812), "812MB");
    }

    #[test]
    fn truncate_respects_width() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 8), "hello w…");
        assert_eq!(width(&truncate("hello world", 8)), 8);
        // Wide characters must not overflow the budget.
        assert!(width(&truncate("日本語テキスト", 5)) <= 5);
    }

    #[test]
    fn truncate_start_keeps_tail() {
        assert_eq!(truncate_start("registry.io/team/app", 10), "…/team/app");
        assert!(width(&truncate_start("registry.io/team/app", 10)) <= 10);
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration_short(45), "45s");
        assert_eq!(duration_short(60 * 5), "5m");
        assert_eq!(duration_short(3600 * 5), "5h");
        assert_eq!(duration_short(86_400 * 2), "2d");
    }
}
