//! Account-level rate-limit usage, fetched from the Anthropic API.
//!
//! Claude Code's `/usage` command reads the account's rolling 5-hour ("session")
//! and 7-day ("week") rate-limit windows from an authenticated endpoint; the data
//! is not persisted to disk. We mirror that call here, reading the OAuth token
//! Claude Code stores in `~/.claude/.credentials.json`.

use std::time::Duration;

use serde_json::Value;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// A single rate-limit window (session or week).
#[derive(Debug, Clone)]
pub struct UsageWindow {
    /// Utilization normalized to 0..=100.
    pub utilization: f64,
    /// When the window resets, as Unix epoch seconds.
    pub resets_at: Option<i64>,
}

/// The account's current usage across the windows we display.
#[derive(Debug, Clone, Default)]
pub struct AccountUsage {
    /// The rolling 5-hour window.
    pub session: Option<UsageWindow>,
    /// The rolling 7-day window.
    pub week: Option<UsageWindow>,
}

impl UsageWindow {
    /// Utilization as a whole-number percentage, clamped to a sane range.
    pub fn percent(&self) -> u32 {
        (self.utilization.round() as i64).clamp(0, 999) as u32
    }

    /// Human-readable time until reset (e.g. "3h12m", "2d4h", "1m"), relative to
    /// `now_secs` (Unix epoch seconds). `None` when unknown or already elapsed.
    pub fn resets_in(&self, now_secs: i64) -> Option<String> {
        let secs = self.resets_at? - now_secs;
        if secs <= 0 {
            return None;
        }
        let mins = secs / 60;
        if mins < 60 {
            Some(format!("{}m", mins.max(1)))
        } else if mins < 60 * 24 {
            Some(format!("{}h{}m", mins / 60, mins % 60))
        } else {
            let hours = mins / 60;
            Some(format!("{}d{}h", hours / 24, hours % 24))
        }
    }
}

/// Fetch account usage from the Anthropic API. Returns `None` when the token is
/// unavailable, the request fails, or no usable window data is returned.
pub fn fetch() -> Option<AccountUsage> {
    let token = read_access_token()?;

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();

    let resp = agent
        .get(USAGE_URL)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("anthropic-version", "2023-06-01")
        .set("User-Agent", "claude-tmux")
        .call()
        .ok()?;

    let body = resp.into_string().ok()?;
    let value: Value = serde_json::from_str(&body).ok()?;

    let usage = AccountUsage {
        session: value.get("five_hour").and_then(parse_window),
        week: value.get("seven_day").and_then(parse_window),
    };

    // Treat "no windows at all" as no usable data.
    if usage.session.is_none() && usage.week.is_none() {
        return None;
    }
    Some(usage)
}

/// Read the OAuth access token from Claude Code's credential store.
fn read_access_token() -> Option<String> {
    let path = dirs::home_dir()?.join(".claude/.credentials.json");
    let contents = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&contents).ok()?;
    value
        .get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()
        .map(str::to_string)
}

/// Normalize a utilization value that may be encoded as a 0–1 fraction or a
/// 0–100 percentage into 0..=100.
fn normalize(u: f64) -> f64 {
    if u <= 1.0 {
        u * 100.0
    } else {
        u
    }
}

/// Parse a single window object (`{"utilization": .., "resets_at": ..}`).
fn parse_window(v: &Value) -> Option<UsageWindow> {
    if !v.is_object() {
        return None;
    }
    let utilization = normalize(v.get("utilization").and_then(Value::as_f64).unwrap_or(0.0));
    let resets_at = v.get("resets_at").and_then(parse_epoch_secs);
    Some(UsageWindow {
        utilization,
        resets_at,
    })
}

/// Parse a reset timestamp into Unix epoch **seconds**, tolerating the shapes an
/// API might return: an integer or float number, a numeric string, or an
/// ISO-8601 / RFC-3339 string. Values that look like milliseconds are scaled
/// down. Returns `None` when nothing usable is present.
fn parse_epoch_secs(v: &Value) -> Option<i64> {
    // JSON number (integer or float). `as_i64` alone misses floats like 1.78e9.
    if let Some(n) = v.as_f64() {
        return Some(scale_epoch(n as i64));
    }
    if let Some(s) = v.as_str() {
        let s = s.trim();
        if let Ok(n) = s.parse::<f64>() {
            return Some(scale_epoch(n as i64));
        }
        return parse_iso8601(s);
    }
    None
}

/// Treat implausibly large values as milliseconds and convert to seconds.
fn scale_epoch(n: i64) -> i64 {
    // Seconds are ~1.7e9 in this era; milliseconds ~1.7e12.
    if n.abs() >= 1_000_000_000_000 {
        n / 1000
    } else {
        n
    }
}

/// Minimal ISO-8601 / RFC-3339 parser for `YYYY-MM-DDThh:mm:ss[.frac][Z|±hh:mm]`.
/// Returns Unix epoch seconds. Best-effort: returns `None` on any malformed part.
fn parse_iso8601(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    // Date and time are separated by 'T' (or a space).
    let (date, rest) = s.split_once(['T', ' '])?;
    let mut dparts = date.split('-');
    let year: i64 = dparts.next()?.parse().ok()?;
    let month: i64 = dparts.next()?.parse().ok()?;
    let day: i64 = dparts.next()?.parse().ok()?;

    // Time is the first 8 chars "hh:mm:ss"; anything after is fraction/offset.
    let hh: i64 = rest.get(0..2)?.parse().ok()?;
    let mm: i64 = rest.get(3..5)?.parse().ok()?;
    let ss: i64 = rest.get(6..8)?.parse().ok()?;

    // Timezone offset: default UTC. Look for a trailing 'Z' or ±hh:mm.
    let mut offset_secs: i64 = 0;
    if let Some(idx) = rest.find(['+', '-']) {
        let sign = if rest.as_bytes()[idx] == b'-' { -1 } else { 1 };
        let tz = &rest[idx + 1..];
        let oh: i64 = tz.get(0..2).and_then(|x| x.parse().ok()).unwrap_or(0);
        let om: i64 = tz.get(3..5).and_then(|x| x.parse().ok()).unwrap_or(0);
        offset_secs = sign * (oh * 3600 + om * 60);
    }

    let days = days_from_civil(year, month, day);
    Some(days * 86400 + hh * 3600 + mm * 60 + ss - offset_secs)
}

/// Days since the Unix epoch (1970-01-01) for a proleptic-Gregorian date.
/// Howard Hinnant's `days_from_civil` algorithm.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_utilization() {
        assert_eq!(normalize(0.34), 34.0);
        assert_eq!(normalize(34.0), 34.0);
        assert_eq!(normalize(0.0), 0.0);
        assert_eq!(normalize(1.0), 100.0);
    }

    #[test]
    fn parses_window() {
        let v: Value =
            serde_json::from_str(r#"{"utilization":0.58,"resets_at":1783585697}"#).unwrap();
        let w = parse_window(&v).unwrap();
        assert_eq!(w.percent(), 58);
        assert_eq!(w.resets_at, Some(1783585697));
    }

    #[test]
    fn parse_window_rejects_non_object() {
        let v: Value = serde_json::from_str("42").unwrap();
        assert!(parse_window(&v).is_none());
    }

    #[test]
    fn parses_resets_at_across_formats() {
        // Integer epoch seconds.
        assert_eq!(parse_epoch_secs(&serde_json::json!(1783585697)), Some(1783585697));
        // Float epoch seconds (the case as_i64 silently dropped before).
        assert_eq!(parse_epoch_secs(&serde_json::json!(1783585697.0)), Some(1783585697));
        // Epoch milliseconds scaled down to seconds.
        assert_eq!(
            parse_epoch_secs(&serde_json::json!(1783585697000i64)),
            Some(1783585697)
        );
        // Numeric string.
        assert_eq!(parse_epoch_secs(&serde_json::json!("1783585697")), Some(1783585697));
        // Non-timestamp inputs.
        assert_eq!(parse_epoch_secs(&serde_json::json!(null)), None);
        assert_eq!(parse_epoch_secs(&serde_json::json!("not a date")), None);
    }

    #[test]
    fn parses_iso8601_timestamps() {
        // 2026-07-09T08:20:05Z == 1783585205 (verified via epoch conversion).
        assert_eq!(parse_iso8601("2026-07-09T08:20:05Z"), Some(1783585205));
        // Fractional seconds are ignored.
        assert_eq!(parse_iso8601("2026-07-09T08:20:05.249Z"), Some(1783585205));
        // Positive offset is subtracted back to UTC.
        assert_eq!(
            parse_iso8601("2026-07-09T10:20:05+02:00"),
            Some(1783585205)
        );
        // The Unix epoch itself.
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn window_parses_float_resets_at() {
        let v: Value =
            serde_json::from_str(r#"{"utilization":34.0,"resets_at":1783585697.0}"#).unwrap();
        let w = parse_window(&v).unwrap();
        assert_eq!(w.percent(), 34);
        assert_eq!(w.resets_at, Some(1783585697));
    }

    #[test]
    fn formats_resets_in() {
        let now = 1_000_000;
        // 3h12m ahead
        let w = UsageWindow {
            utilization: 0.0,
            resets_at: Some(now + 3 * 3600 + 12 * 60),
        };
        assert_eq!(w.resets_in(now).as_deref(), Some("3h12m"));

        // 90s ahead -> rounds up to at least 1m
        let w = UsageWindow {
            utilization: 0.0,
            resets_at: Some(now + 90),
        };
        assert_eq!(w.resets_in(now).as_deref(), Some("1m"));

        // 2d4h ahead
        let w = UsageWindow {
            utilization: 0.0,
            resets_at: Some(now + 2 * 86400 + 4 * 3600),
        };
        assert_eq!(w.resets_in(now).as_deref(), Some("2d4h"));

        // already elapsed
        let w = UsageWindow {
            utilization: 0.0,
            resets_at: Some(now - 10),
        };
        assert_eq!(w.resets_in(now), None);
    }
}
