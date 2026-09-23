//! Zero-config live evidence fetch for SearchAgent.
//! Uses public HTTP APIs (Open-Meteo, DuckDuckGo) — no vendor keys required.

use susi_error::{EaiError, EaiResult};

const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

fn http_client() -> EaiResult<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent("susi/0.4 (+https://github.com/intellibitz/susi)")
        .build()
        .map_err(|e| EaiError::process(format!("HTTP client init failed: {e}")))
}

pub fn looks_like_weather_goal(goal: &str) -> bool {
    let lower = goal.to_ascii_lowercase();
    [
        "weather",
        "temperature",
        "forecast",
        "humidity",
        "wind speed",
        "precipitation",
        "rainfall",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

/// Pull a place name from common phrasings ("weather in Chennai, India").
pub fn extract_place(goal: &str) -> Option<String> {
    let lower = goal.to_ascii_lowercase();
    for needle in [
        " weather in ",
        " weather for ",
        " forecast for ",
        " forecast in ",
        " temperature in ",
        " temperature for ",
        " in ",
        " for ",
        " at ",
    ] {
        if let Some(idx) = lower.find(needle) {
            let rest = goal[idx + needle.len()..].trim();
            let mut place = String::new();
            for ch in rest.chars() {
                if ch.is_ascii_alphabetic() || ch == ' ' || ch == ',' || ch == '-' || ch == '\'' {
                    place.push(ch);
                } else {
                    break;
                }
            }
            let place = place
                .trim()
                .trim_matches(|c: char| c == ',' || c == '-' || c.is_whitespace())
                .to_string();
            // Drop trailing mission instructions accidentally captured.
            let place = place
                .split(',')
                .take(2)
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            if place.len() >= 2 && !place.to_ascii_lowercase().starts_with("this ") {
                return Some(place);
            }
        }
    }
    None
}

fn wmo_label(code: i64) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51..=57 => "drizzle",
        61..=67 => "rain",
        71..=77 => "snow",
        80..=82 => "rain showers",
        85 | 86 => "snow showers",
        95..=99 => "thunderstorm",
        _ => "see WMO weather code",
    }
}

/// Live observation via Open-Meteo (no API key).
pub fn fetch_open_meteo_weather(place: &str) -> EaiResult<String> {
    let client = http_client()?;
    let geo_url = format!(
        "https://geocoding-api.open-meteo.com/v1/search?name={}&count=1&language=en&format=json",
        urlencoding_lite(place)
    );
    let geo: serde_json::Value = client
        .get(&geo_url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| EaiError::process(format!("Open-Meteo geocoding failed: {e}")))?;

    let result = geo
        .get("results")
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| {
            EaiError::process(format!(
                "Open-Meteo geocoding found no place matching '{place}'"
            ))
        })?;

    let lat = result
        .get("latitude")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| EaiError::process("geocode missing latitude"))?;
    let lon = result
        .get("longitude")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| EaiError::process("geocode missing longitude"))?;
    let resolved = result.get("name").and_then(|v| v.as_str()).unwrap_or(place);
    let country = result.get("country").and_then(|v| v.as_str()).unwrap_or("");
    let admin1 = result.get("admin1").and_then(|v| v.as_str()).unwrap_or("");
    let tz = result
        .get("timezone")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");

    let wx_url = format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}&current=temperature_2m,relative_humidity_2m,weather_code,wind_speed_10m&timezone={}",
        urlencoding_lite(tz)
    );
    let wx: serde_json::Value = client
        .get(&wx_url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| EaiError::process(format!("Open-Meteo forecast failed: {e}")))?;

    let current = wx
        .get("current")
        .ok_or_else(|| EaiError::process("Open-Meteo response missing current block"))?;
    let obs_time = current
        .get("time")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let temp = current
        .get("temperature_2m")
        .and_then(|v| v.as_f64())
        .map(|t| format!("{t:.1}"))
        .unwrap_or_else(|| "?".into());
    let humidity = current
        .get("relative_humidity_2m")
        .and_then(|v| v.as_f64())
        .map(|h| format!("{h:.0}"))
        .unwrap_or_else(|| "?".into());
    let wind = current
        .get("wind_speed_10m")
        .and_then(|v| v.as_f64())
        .map(|w| format!("{w:.1}"))
        .unwrap_or_else(|| "?".into());
    let code = current
        .get("weather_code")
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    let location = [resolved, admin1, country]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ");

    Ok(format!(
        "**Live weather observation**\n\
         - Location: {location} ({lat:.4}, {lon:.4})\n\
         - Observation time: {obs_time} ({tz})\n\
         - Temperature: {temp} °C\n\
         - Relative humidity: {humidity} %\n\
         - Wind speed: {wind} km/h\n\
         - Conditions: {} (WMO code {code})\n\
         - Source: Open-Meteo API (https://open-meteo.com/) — free public forecast endpoint, no API key\n\
         - Evidence URLs: geocode `{geo_url}` ; forecast `{wx_url}`",
        wmo_label(code)
    ))
}

/// DuckDuckGo Instant Answer (zero-config) for non-weather factual lookups.
pub fn fetch_duckduckgo_instant(query: &str) -> EaiResult<String> {
    let client = http_client()?;
    let url = format!(
        "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
        urlencoding_lite(query)
    );
    let body: serde_json::Value = client
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| EaiError::process(format!("DuckDuckGo lookup failed: {e}")))?;

    let heading = body
        .get("Heading")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let abstract_text = body
        .get("AbstractText")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let abstract_url = body
        .get("AbstractURL")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    let answer = body
        .get("Answer")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();

    if abstract_text.is_empty() && answer.is_empty() {
        return Err(EaiError::process(
            "DuckDuckGo returned no Instant Answer abstract",
        ));
    }

    let mut out = String::from("**Live web evidence (DuckDuckGo Instant Answer)**\n");
    if !heading.is_empty() {
        out.push_str(&format!("- Topic: {heading}\n"));
    }
    if !answer.is_empty() {
        out.push_str(&format!("- Answer: {answer}\n"));
    }
    if !abstract_text.is_empty() {
        out.push_str(&format!("- Abstract: {abstract_text}\n"));
    }
    if !abstract_url.is_empty() {
        out.push_str(&format!("- Source URL: {abstract_url}\n"));
    }
    out.push_str(&format!("- Evidence URL: {url}\n"));
    Ok(out)
}

/// Prefer live weather, then MCP search tools, then DuckDuckGo; never invent.
/// Every fetch is captured into the mission's evidence ledger (when one is
/// live for `workspace`), so downstream answers can cite real receipts.
pub fn gather_live_evidence(goal: &str, workspace: &std::path::Path) -> String {
    let mut attempts: Vec<String> = Vec::new();

    if looks_like_weather_goal(goal) {
        match extract_place(goal) {
            Some(place) => {
                let args = serde_json::json!({ "place": place });
                match susi_core::capture::EvidenceSession::capture_call(
                    "open_meteo_weather",
                    &args,
                    workspace,
                    || fetch_open_meteo_weather(&place),
                ) {
                    Ok(report) => return report,
                    Err(e) => attempts.push(format!("Open-Meteo({place}): {e}")),
                }
            }
            None => {
                attempts.push("Open-Meteo: could not extract a place name from the goal".into())
            }
        }
    }

    for tool in ["brave_search", "google_search", "web_search"] {
        if susi_core::plane_bus::tools::exists(tool) {
            let args = serde_json::json!({ "query": goal, "q": goal });
            let out = susi_core::plane_bus::tools::execute_tool(tool, &args, workspace)
                .unwrap_or_default();
            let lower = out.to_ascii_lowercase();
            if !out.trim().is_empty()
                && !lower.contains("error")
                && !lower.contains("not found")
                && !lower.contains("capability_gap")
            {
                return format!(
                    "**Live search evidence via `{tool}`**\n{out}\n- Observation time: system clock at fetch"
                );
            }
            attempts.push(format!(
                "{tool}: unusable response ({})",
                out.chars().take(120).collect::<String>()
            ));
        } else {
            attempts.push(format!("{tool}: not linked"));
        }
    }

    match susi_core::capture::EvidenceSession::capture_call(
        "duckduckgo_instant",
        &serde_json::json!({ "query": goal }),
        workspace,
        || fetch_duckduckgo_instant(goal),
    ) {
        Ok(report) => return report,
        Err(e) => attempts.push(format!("DuckDuckGo: {e}")),
    }

    format!(
        "**Live search/weather data unavailable.**\n\
         No verifiable live observation could be fetched. Attempts:\n{}\n\
         I will not invent conditions or citations.",
        attempts
            .iter()
            .map(|a| format!("- {a}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Minimal URL-encoding for query components (space → %20, reserve ASCII).
fn urlencoding_lite(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push_str("%20"),
            b',' => out.push_str("%2C"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_chennai_from_weather_goal() {
        let place = extract_place(
            "What is the current weather in Chennai, India? Fetch live weather evidence.",
        );
        assert_eq!(place.as_deref(), Some("Chennai, India"));
    }

    #[test]
    fn weather_goal_detection() {
        assert!(looks_like_weather_goal("current weather in Paris"));
        assert!(!looks_like_weather_goal("refactor the auth module"));
    }

    #[test]
    fn open_meteo_chennai_smoke() {
        // Network-dependent; skip quietly when offline.
        match fetch_open_meteo_weather("Chennai, India") {
            Ok(report) => {
                assert!(report.contains("Live weather observation"));
                assert!(report.contains("Open-Meteo"));
                assert!(report.contains("Observation time:"));
                assert!(report.contains("Temperature:"));
            }
            Err(e) => {
                eprintln!("skipping open_meteo_chennai_smoke (network): {e}");
            }
        }
    }
}
