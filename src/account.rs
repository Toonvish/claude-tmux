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
    let resets_at = v.get("resets_at").and_then(Value::as_i64);
    Some(UsageWindow {
        utilization,
        resets_at,
    })
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
