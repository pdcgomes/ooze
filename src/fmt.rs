use std::time::Duration;

/// "3 min", "5 h", "4 days", "3 months".
pub fn age(d: Duration) -> String {
    let s = d.as_secs();
    let plural = |n: u64, unit: &str| if n == 1 { format!("1 {unit}") } else { format!("{n} {unit}s") };
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min", s / 60),
        3600..=172_799 => format!("{} h", s / 3600),
        172_800..=5_183_999 => plural(s / 86_400, "day"),
        5_184_000..=63_071_999 => plural(s / 2_592_000, "month"),
        _ => plural(s / 31_536_000, "year"),
    }
}

pub fn ago(d: Duration) -> String {
    let a = age(d);
    if a == "just now" { a } else { format!("{a} ago") }
}

/// "12 s", "4 min", "3 h 12 min": how long something has lasted.
pub fn span(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", s / 60),
        _ if s % 3600 / 60 == 0 => format!("{} h", s / 3600),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

/// "2:14", "12:03", "1:04:09": a running build's clock.
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=3599 => format!("{}:{:02}", s / 60, s % 60),
        _ => format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60),
    }
}

/// "22,882".
pub fn num(n: u64) -> String {
    let digits = n.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    grouped
}

/// "9,999", "12.3k", "123k", "1.2M": a count for a narrow column.
pub fn compact(n: u64) -> String {
    match n {
        0..=9_999 => num(n),
        10_000..=99_999 => format!("{:.1}k", n as f64 / 1e3),
        100_000..=999_999 => format!("{}k", (n as f64 / 1e3).round()),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

pub fn count(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", num(n), if n == 1 { one } else { many })
}

/// "$25.31", "$1,204", "<$0.01", "$0", "−$5.31".
pub fn money(usd: f64) -> String {
    if usd < 0.0 {
        return format!("−{}", money(-usd));
    }
    if usd == 0.0 {
        "$0".into()
    } else if usd < 0.005 {
        "<$0.01".into()
    } else if usd < 1_000.0 {
        format!("${usd:.2}")
    } else {
        format!("${}", num(usd.round() as u64))
    }
}

/// "94%", "1.2%", "<0.1%", "0%": a share, from 0 to 1.
pub fn percent(ratio: f64) -> String {
    let p = ratio * 100.0;
    if p <= 0.0 {
        "0%".into()
    } else if p < 0.1 {
        "<0.1%".into()
    } else if p < 10.0 {
        format!("{p:.1}%")
    } else {
        format!("{}%", p.round())
    }
}

/// Milliseconds between two times, as a duration, never negative.
pub fn between(from: i64, to: i64) -> Duration {
    Duration::from_millis((to - from).max(0) as u64)
}

/// "18:02", or "yesterday 18:02", or "12 Oct 18:02", in local time.
pub fn when(at: i64, now: i64) -> String {
    use chrono::{Local, TimeZone};
    let (Some(at), Some(now)) = (Local.timestamp_millis_opt(at).single(), Local.timestamp_millis_opt(now).single()) else {
        return String::new();
    };
    let days = (now.date_naive() - at.date_naive()).num_days();
    match days {
        0 => at.format("%H:%M").to_string(),
        1 => at.format("yesterday %H:%M").to_string(),
        _ => at.format("%-d %b %H:%M").to_string(),
    }
}

/// "18:02:41", in local time.
pub fn time(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at)
        .map(|d| d.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// "3 Nov", in local time.
pub fn day(at: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at)
        .map(|d| d.with_timezone(&chrono::Local).format("%-d %b").to_string())
        .unwrap_or_default()
}

/// This machine's time zone as an IANA name, such as "Europe/London", for
/// asking Vercel for days that start at local midnight.
pub fn local_timezone() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':');
        if tz.contains('/') {
            return Some(tz.to_string());
        }
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let s = target.to_string_lossy();
    s.split_once("zoneinfo/").map(|(_, name)| name.to_string())
}

/// Shortens from the end, for names whose start matters.
pub fn fit_end(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let head: String = s.chars().take(w - 1).collect();
    format!("{head}…")
}

/// Padded or cut to exactly `w` columns.
pub fn pad(s: &str, w: usize) -> String {
    let s = fit_end(s, w);
    let n = s.chars().count();
    format!("{s}{}", " ".repeat(w.saturating_sub(n)))
}

/// Right-aligned in exactly `w` columns.
pub fn rpad(s: &str, w: usize) -> String {
    let s = fit_end(s, w);
    let n = s.chars().count();
    format!("{}{s}", " ".repeat(w.saturating_sub(n)))
}

/// The first line of something, with runs of white space as one space.
pub fn one_line(s: &str) -> String {
    s.lines().next().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A commit's short form.
pub fn sha(s: &str) -> String {
    s.chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_and_clocks() {
        assert_eq!(span(Duration::from_secs(12)), "12 s");
        assert_eq!(span(Duration::from_secs(4 * 60 + 3)), "4 min");
        assert_eq!(span(Duration::from_secs(3 * 3600 + 12 * 60)), "3 h 12 min");
        assert_eq!(clock(Duration::from_secs(72)), "1:12");
        assert_eq!(clock(Duration::from_secs(3849)), "1:04:09");
        assert_eq!(ago(Duration::from_secs(7_300)), "2 h ago");
        assert_eq!(ago(Duration::from_secs(20)), "just now");
    }

    #[test]
    fn counts_money_and_shares() {
        assert_eq!(num(1_204), "1,204");
        assert_eq!(compact(3_410), "3,410");
        assert_eq!(compact(12_345), "12.3k");
        assert_eq!(compact(123_456), "123k");
        assert_eq!(compact(1_234_567), "1.2M");
        assert_eq!(money(25.314), "$25.31");
        assert_eq!(money(1_204.4), "$1,204");
        assert_eq!(money(0.001), "<$0.01");
        assert_eq!(money(0.0), "$0");
        assert_eq!(money(-5.31), "−$5.31");
        assert_eq!(percent(0.94), "94%");
        assert_eq!(percent(0.012), "1.2%");
        assert_eq!(percent(0.0004), "<0.1%");
        assert_eq!(percent(0.0), "0%");
    }

    #[test]
    fn fitting() {
        assert_eq!(fit_end("night-market", 6), "night…");
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(rpad("12", 4), "  12");
        assert_eq!(pad("abcdef", 4), "abc…");
        assert_eq!(one_line("Fix the  search\n\nIt was slow"), "Fix the search");
        assert_eq!(sha("a1b2c3d4e5"), "a1b2c3d");
    }
}
