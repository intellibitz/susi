//! Quota evidence: normal rate limits never imply a free billing allowance.
//! Header semantics: https://platform.claude.com/docs/en/api/rate-limits
//! and https://developers.openai.com/api/docs/guides/rate-limits.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Amount {
    #[default]
    Unknown,
    Limited(u64),
    Unlimited,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    pub requests: Amount,
    pub input_tokens: Amount,
    pub output_tokens: Amount,
    pub request_reset_at: Option<u64>,
    pub input_reset_at: Option<u64>,
    pub output_reset_at: Option<u64>,
    pub retry_at: Option<u64>,
    /// Unknown unless supported billing metadata explicitly reports it.
    pub free_requests: Amount,
    pub credit_microusd: Amount,
    pub key_spend_remaining_microusd: Amount,
    pub account_free_tier: Option<bool>,
}

impl Quota {
    /// Do not invent replenished quotas at reset; reset only permits fresh
    /// discovery. A throttle deadline is authoritative until it expires.
    pub fn blocked_until(&self, now: u64) -> Option<u64> {
        [
            self.retry_at,
            self.request_reset_at
                .filter(|_| self.requests == Amount::Limited(0)),
            self.input_reset_at
                .filter(|_| self.input_tokens == Amount::Limited(0)),
            self.output_reset_at
                .filter(|_| self.output_tokens == Amount::Limited(0)),
        ]
        .into_iter()
        .flatten()
        .filter(|t| *t > now)
        .max()
    }
}

/// Parse an RFC3339 UTC timestamp without guessing numeric units or zones.
fn timestamp(value: &str) -> Option<u64> {
    let (date, clock) = value.split_once('T')?;
    let clock = clock.strip_suffix('Z')?;
    let clock = clock.split('.').next()?;
    let mut parts = clock.split(':');
    let hour: u64 = parts.next()?.parse().ok()?;
    let minute: u64 = parts.next()?.parse().ok()?;
    let second: u64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = u64::try_from(crate::eco_schema::date_to_days(date)?).ok()?;
    days.checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)
}

/// OpenAI reset durations can contain several units (e.g. `1m2.5s`).
/// Round fractional seconds upward so early retries cannot exceed a limit.
fn duration(value: &str) -> Option<u64> {
    if value.is_empty() {
        return None;
    }
    let mut rest = value;
    let mut millis = 0u64;
    while !rest.is_empty() {
        let end = rest.find(|c: char| !c.is_ascii_digit() && c != '.')?;
        let number = &rest[..end];
        rest = &rest[end..];
        let (unit, scale) = if rest.starts_with("ms") {
            (2, 1)
        } else if rest.starts_with('s') {
            (1, 1000)
        } else if rest.starts_with('m') {
            (1, 60_000)
        } else if rest.starts_with('h') {
            (1, 3_600_000)
        } else if rest.starts_with('d') {
            (1, 86_400_000)
        } else {
            return None;
        };
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        let whole = whole.parse::<u64>().ok()?.checked_mul(scale)?;
        if fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let fraction = if fraction.is_empty() {
            0
        } else {
            let divisor = 10u64.checked_pow(fraction.len() as u32)?;
            fraction
                .parse::<u64>()
                .ok()?
                .checked_mul(scale)?
                .div_ceil(divisor)
        };
        millis = millis.checked_add(whole)?.checked_add(fraction)?;
        rest = &rest[unit..];
    }
    Some(millis.div_ceil(1000))
}

fn http_date(value: &str) -> Option<u64> {
    let fields: Vec<_> = value.split_whitespace().collect();
    if fields.len() != 6 || fields[5] != "GMT" {
        return None;
    }
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == fields[2])?
        + 1;
    let day = fields[1].parse::<u32>().ok()?;
    let year = fields[3].parse::<u32>().ok()?;
    timestamp(&format!("{year:04}-{month:02}-{day:02}T{}Z", fields[4]))
}

pub fn from_headers(get: impl Fn(&str) -> Option<String>, now: u64) -> Quota {
    let amount = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| get(name))
            .and_then(|v| v.trim().parse().ok())
            .map_or(Amount::Unknown, Amount::Limited)
    };
    let server_now = get("date").and_then(|v| http_date(&v));
    let absolute = |time: u64| {
        server_now.map_or(Some(time), |server| {
            now.checked_add(time.saturating_sub(server))
        })
    };
    let reset = |anthropic: &str, openai: &str| {
        get(anthropic).and_then(|v| timestamp(&v)).or_else(|| {
            get(openai)
                .and_then(|v| duration(&v))
                .and_then(|t| now.checked_add(t))
        })
    };
    Quota {
        requests: amount(&[
            "anthropic-ratelimit-requests-remaining",
            "x-ratelimit-remaining-requests",
        ]),
        input_tokens: amount(&[
            "anthropic-ratelimit-input-tokens-remaining",
            "x-ratelimit-remaining-tokens",
        ]),
        output_tokens: amount(&["anthropic-ratelimit-output-tokens-remaining"]),
        request_reset_at: reset(
            "anthropic-ratelimit-requests-reset",
            "x-ratelimit-reset-requests",
        ),
        input_reset_at: reset(
            "anthropic-ratelimit-input-tokens-reset",
            "x-ratelimit-reset-tokens",
        ),
        output_reset_at: reset("anthropic-ratelimit-output-tokens-reset", ""),
        retry_at: get("retry-after").and_then(|v| {
            v.trim()
                .parse::<u64>()
                .ok()
                .and_then(|t| now.checked_add(t))
                .or_else(|| http_date(&v).and_then(absolute))
        }),
        ..Quota::default()
    }
}

/// OpenRouter /key metadata reports a key spending cap, not account balance.
/// Source: https://openrouter.ai/docs/api/api-reference/api-keys/get-current-key
pub fn openrouter_metadata(value: &serde_json::Value) -> Quota {
    let data = &value["data"];
    let spend = data["limit_remaining"]
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0 && *v < u64::MAX as f64 / 1_000_000.0)
        .map(|v| Amount::Limited((v * 1_000_000.0).floor() as u64))
        .unwrap_or_default();
    Quota {
        key_spend_remaining_microusd: spend,
        account_free_tier: data["is_free_tier"].as_bool(),
        ..Quota::default()
    }
}

/// Account credits are separate from a key's spending cap. This metadata
/// requires management-key access; 403 is unknown, never a revoked key.
/// Source: https://openrouter.ai/docs/api/api-reference/credits/get-credits
pub fn openrouter_credits(value: &serde_json::Value) -> Amount {
    let purchased = value["data"]["total_credits"].as_f64();
    let used = value["data"]["total_usage"].as_f64();
    match (purchased, used) {
        (Some(p), Some(u))
            if p.is_finite()
                && u.is_finite()
                && p >= 0.0
                && u >= 0.0
                && p < u64::MAX as f64 / 1_000_000.0 =>
        {
            Amount::Limited(((p - u).max(0.0) * 1_000_000.0).floor() as u64)
        }
        _ => Amount::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn headers(rows: &[(&str, &str)], now: u64) -> Quota {
        from_headers(
            |name| {
                rows.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| (*v).into())
            },
            now,
        )
    }
    #[test]
    fn cloud_quota_inventory_known_zero_is_not_unknown_or_free() {
        let q = headers(
            &[
                ("X-RateLimit-Remaining-Requests", "0"),
                ("x-ratelimit-reset-requests", "1m2.5s"),
            ],
            100,
        );
        assert_eq!(q.requests, Amount::Limited(0));
        assert_eq!(q.request_reset_at, Some(163));
        assert_eq!(q.blocked_until(101), Some(163));
        assert_eq!(q.blocked_until(163), None);
        assert_eq!(q.free_requests, Amount::Unknown);
        assert_eq!(q.credit_microusd, Amount::Unknown);
        assert_eq!(headers(&[], 100).requests, Amount::Unknown);
    }
    #[test]
    fn cloud_quota_inventory_anthropic_reset_and_retry_after() {
        let q = headers(
            &[
                ("anthropic-ratelimit-input-tokens-remaining", "0"),
                (
                    "anthropic-ratelimit-input-tokens-reset",
                    "2026-09-30T12:00:00Z",
                ),
                ("retry-after", "5"),
            ],
            100,
        );
        assert_eq!(q.input_reset_at, Some(1_790_769_600));
        assert_eq!(q.retry_at, Some(105));
        assert_eq!(q.blocked_until(100), q.input_reset_at);
        assert_eq!(
            headers(
                &[
                    ("retry-after", "invalid"),
                    ("x-ratelimit-reset-tokens", "infinity")
                ],
                100
            )
            .retry_at,
            None
        );
    }
    #[test]
    fn cloud_quota_inventory_billing_metadata_never_invents_balance_or_free_allowance() {
        for free in [true, false] {
            let q = openrouter_metadata(
                &serde_json::json!({"data":{"is_free_tier":free,"limit_remaining":12.5}}),
            );
            assert_eq!(q.account_free_tier, Some(free));
            assert_eq!(q.key_spend_remaining_microusd, Amount::Limited(12_500_000));
            assert_eq!(q.credit_microusd, Amount::Unknown);
            assert_eq!(q.free_requests, Amount::Unknown);
        }
        assert_eq!(
            openrouter_metadata(&serde_json::json!({"data":{"limit_remaining":null}}))
                .key_spend_remaining_microusd,
            Amount::Unknown
        );
    }

    #[test]
    fn cloud_quota_inventory_clock_skew_and_balance_are_conservative() {
        let q = headers(
            &[
                ("date", "Wed, 30 Sep 2026 12:00:00 GMT"),
                ("retry-after", "Wed, 30 Sep 2026 12:00:10 GMT"),
            ],
            100,
        );
        assert_eq!(q.retry_at, Some(110));
        assert_eq!(
            openrouter_credits(
                &serde_json::json!({"data":{"total_credits":10.5,"total_usage":3.25}})
            ),
            Amount::Limited(7_250_000)
        );
        assert_eq!(
            openrouter_credits(
                &serde_json::json!({"data":{"total_credits":0.0,"total_usage":3.25}})
            ),
            Amount::Limited(0)
        );
        assert_eq!(
            openrouter_credits(&serde_json::json!({"error":{"message":"unauthorized"}})),
            Amount::Unknown
        );
    }

    #[test]
    fn cloud_quota_inventory_duration_overflow_and_malformed_remain_unknown() {
        for v in [
            "",
            "1",
            "-1s",
            "NaNs",
            "99999999999999999999999s",
            "1.2345s",
        ] {
            assert_eq!(duration(v), None);
        }
        assert_eq!(duration("2ms"), Some(1));
        assert_eq!(timestamp("2026-02-30T00:00:00Z"), None);
        assert_eq!(timestamp("2026-09-30T25:00:00Z"), None);
    }
}
