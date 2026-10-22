//! The public ingress in front of the metering gateway.
//!
//! The gateway answers anyone who can reach its loopback port. This layer is
//! what makes it safe to expose: a bearer key names an account, the account
//! carries a money cap and a rate limit, and every admitted call is forwarded
//! to [`crate::gateway::complete`] with the account recorded as the mission —
//! so the ledger already has the per-key spend the next admission reads.
//!
//! Admission is pessimistic, like the mission budget: the prompt is priced at
//! the cache-miss rate and the whole output allowance at the output rate, so a
//! short answer can only make the real charge smaller. An account with no
//! `budget_usd` has no money cap; an ingress with no accounts is open (the
//! loopback-only default).

use crate::budget::{Budget, Spend};
use crate::gateway::{complete, GatewayState};
use crate::money::MicroUsd;
use crate::providers::Registry;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use susi_error::{EaiError, EaiResult};

/// A caller allowed to use the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Bearer token the caller presents.
    pub token: String,
    /// Name recorded as the mission on every ledger row.
    pub name: String,
    /// Most this account may spend, in micro-USD. `None` is uncapped.
    pub budget: Option<MicroUsd>,
    /// Requests allowed per minute. `None` is unlimited.
    pub requests_per_minute: Option<u32>,
}

impl Account {
    /// An account with no caps.
    #[must_use]
    pub fn new(token: &str, name: &str) -> Self {
        Self {
            token: token.to_string(),
            name: name.to_string(),
            budget: None,
            requests_per_minute: None,
        }
    }

    /// Cap the account's spend.
    #[must_use]
    pub fn with_budget(mut self, limit: MicroUsd) -> Self {
        self.budget = Some(limit);
        self
    }

    /// Cap the account's request rate.
    #[must_use]
    pub fn with_rate(mut self, per_minute: u32) -> Self {
        self.requests_per_minute = Some(per_minute);
        self
    }

    /// The account the open ingress reports calls against.
    #[must_use]
    pub fn open() -> Self {
        Self {
            token: String::new(),
            name: "open".to_string(),
            budget: None,
            requests_per_minute: None,
        }
    }
}

/// Why a caller was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// No bearer token was presented.
    MissingToken,
    /// The token names no account.
    UnknownToken,
    /// The account's per-minute limit is spent.
    RateLimited,
    /// The call's worst-case cost would cross the account's cap.
    OverBudget,
}

impl Denial {
    /// A stable, human-readable reason.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::MissingToken => "no bearer token; send `Authorization: Bearer <token>`",
            Self::UnknownToken => "the bearer token names no account",
            Self::RateLimited => "the account's requests per minute are exhausted",
            Self::OverBudget => "the call would cross the account's budget",
        }
    }

    /// The HTTP status that carries the denial.
    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self {
            Self::MissingToken | Self::UnknownToken => StatusCode::UNAUTHORIZED,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::OverBudget => StatusCode::PAYMENT_REQUIRED,
        }
    }
}

/// The accounts and their rate windows.
#[derive(Debug)]
pub struct Ingress {
    accounts: Vec<Account>,
    windows: Mutex<BTreeMap<String, Window>>,
}

#[derive(Debug, Clone, Copy)]
struct Window {
    minute: u64,
    count: u32,
}

impl Ingress {
    /// An ingress over the given accounts. An empty list is open.
    #[must_use]
    pub fn new(accounts: Vec<Account>) -> Self {
        Self {
            accounts,
            windows: Mutex::new(BTreeMap::new()),
        }
    }

    /// The accounts this ingress serves.
    #[must_use]
    pub fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    /// Whether the ingress requires a token.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Parse an accounts document:
    ///
    /// ```json
    /// {"accounts": [
    ///   {"token": "sk-…", "name": "alice", "budget_usd": "2.00",
    ///    "requests_per_minute": 60}
    /// ]}
    /// ```
    ///
    /// # Errors
    ///
    /// Returns a config error for malformed JSON, a missing token, a duplicate
    /// token, a non-numeric budget or a zero rate.
    pub fn parse(json: &str) -> EaiResult<Self> {
        let root: Value = serde_json::from_str(json)
            .map_err(|error| EaiError::config(format!("ingress accounts are not JSON: {error}")))?;
        let raw = root
            .get("accounts")
            .and_then(Value::as_array)
            .ok_or_else(|| EaiError::config("ingress document has no accounts array"))?;
        let mut accounts = Vec::with_capacity(raw.len());
        for entry in raw {
            let account = parse_account(entry)?;
            if accounts
                .iter()
                .any(|known: &Account| known.token == account.token)
            {
                return Err(EaiError::config(format!(
                    "ingress account `{}` repeats a token",
                    account.name
                )));
            }
            accounts.push(account);
        }
        Ok(Self::new(accounts))
    }

    /// Resolve a presented token to an account.
    ///
    /// An empty ingress admits everyone as the `open` account.
    ///
    /// # Errors
    ///
    /// [`Denial::MissingToken`] when no token is presented and accounts exist,
    /// [`Denial::UnknownToken`] when the token names no account.
    pub fn authenticate(&self, token: Option<&str>) -> Result<Account, Denial> {
        if self.accounts.is_empty() {
            return Ok(Account::open());
        }
        let token = token.ok_or(Denial::MissingToken)?;
        self.accounts
            .iter()
            .find(|account| account.token == token)
            .cloned()
            .ok_or(Denial::UnknownToken)
    }

    /// Admit one call: rate window first, then the money cap.
    ///
    /// `estimate` is the call's worst-case cost and `spent` what the ledger
    /// already shows for this account.
    ///
    /// # Errors
    ///
    /// [`Denial::RateLimited`] or [`Denial::OverBudget`].
    pub fn admit(
        &self,
        account: &Account,
        estimate: MicroUsd,
        spent: Spend,
        now_minute: u64,
    ) -> Result<(), Denial> {
        if let Some(limit) = account.requests_per_minute {
            let mut windows = self
                .windows
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let window = windows.entry(account.name.clone()).or_insert(Window {
                minute: now_minute,
                count: 0,
            });
            if window.minute != now_minute {
                window.minute = now_minute;
                window.count = 0;
            }
            if window.count >= limit {
                return Err(Denial::RateLimited);
            }
            window.count = window.count.saturating_add(1);
        }
        if let Some(cap) = account.budget {
            if !Budget::new(cap).admits(spent, MicroUsd::ZERO, estimate) {
                return Err(Denial::OverBudget);
            }
        }
        Ok(())
    }
}

/// Build the public router.
pub fn router(gateway: Arc<GatewayState>, ingress: Arc<Ingress>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(completions))
        .with_state(IngressState { gateway, ingress })
}

/// Serve the ingress on an already-bound listener.
///
/// # Errors
///
/// Returns the listener's I/O error when the server stops.
pub async fn serve(
    listener: tokio::net::TcpListener,
    gateway: Arc<GatewayState>,
    ingress: Arc<Ingress>,
) -> std::io::Result<()> {
    axum::serve(listener, router(gateway, ingress)).await
}

/// The model catalogue as an OpenAI-compatible list, aliases included.
#[must_use]
pub fn models_payload() -> Value {
    let registry = Registry::builtin();
    let mut data = Vec::new();
    for provider in registry.providers() {
        for model in &provider.models {
            data.push(json!({
                "id": model.id,
                "object": "model",
                "owned_by": provider.id,
            }));
        }
    }
    for alias in registry.aliases().keys() {
        data.push(json!({
            "id": alias,
            "object": "model",
            "owned_by": "alias",
        }));
    }
    json!({"object": "list", "data": data})
}

/// The bearer token a request carries, if any.
#[must_use]
pub fn token_from(headers: &HeaderMap) -> Option<&str> {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        let value = value.trim();
        let token = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
            .unwrap_or(value)
            .trim();
        if !token.is_empty() {
            return Some(token);
        }
    }
    headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

#[derive(Clone)]
struct IngressState {
    gateway: Arc<GatewayState>,
    ingress: Arc<Ingress>,
}

async fn health() -> &'static str {
    "ok"
}

async fn models(State(state): State<IngressState>, headers: HeaderMap) -> Response {
    match state.ingress.authenticate(token_from(&headers)) {
        Ok(_) => Json(models_payload()).into_response(),
        Err(denial) => deny(denial),
    }
}

async fn completions(
    State(state): State<IngressState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let account = match state.ingress.authenticate(token_from(&headers)) {
        Ok(account) => account,
        Err(denial) => return deny(denial),
    };
    let spent = Budget::spent(&state.gateway.ledger_path, &account.name);
    let estimate = estimate(&state.gateway, &account, &body);
    if let Err(denial) = state.ingress.admit(&account, estimate, spent, now_minute()) {
        return deny(denial);
    }
    let mut headers = headers;
    if !headers.contains_key("x-susi-mission") {
        if let Ok(value) = HeaderValue::from_str(&account.name) {
            headers.insert("x-susi-mission", value);
        }
    }
    complete(state.gateway.clone(), headers, body).await
}

/// The worst-case cost of this call for an account with a cap.
fn estimate(gateway: &GatewayState, account: &Account, body: &str) -> MicroUsd {
    let Some(cap) = account.budget else {
        return MicroUsd::ZERO;
    };
    let Ok(request) = serde_json::from_str::<Value>(body) else {
        return MicroUsd::ZERO;
    };
    let Some(model) = request.get("model").and_then(Value::as_str) else {
        return MicroUsd::ZERO;
    };
    let Some(price) = gateway.catalogue.price(model) else {
        return MicroUsd::ZERO;
    };
    Budget::new(cap).estimate(price, body)
}

fn deny(denial: Denial) -> Response {
    (
        denial.status(),
        Json(json!({
            "error": {
                "message": denial.message(),
                "type": "susi_ingress",
            }
        })),
    )
        .into_response()
}

fn now_minute() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() / 60)
        .unwrap_or(0)
}

fn parse_account(raw: &Value) -> EaiResult<Account> {
    let token = raw
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| EaiError::config("ingress account has no token"))?
        .to_string();
    let name = raw
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&token)
        .to_string();
    let budget = match raw.get("budget_usd").and_then(Value::as_str) {
        Some(text) => Some(MicroUsd::parse_usd(text).ok_or_else(|| {
            EaiError::config(format!(
                "ingress account `{name}` budget_usd `{text}` is not an amount"
            ))
        })?),
        None => None,
    };
    let requests_per_minute = match raw.get("requests_per_minute").and_then(Value::as_u64) {
        Some(0) => {
            return Err(EaiError::config(format!(
                "ingress account `{name}` allows zero requests per minute"
            )))
        }
        Some(rate) => Some(u32::try_from(rate).unwrap_or(u32::MAX)),
        None => None,
    };
    Ok(Account {
        token,
        name,
        budget,
        requests_per_minute,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account() -> Account {
        Account::new("sk-test", "alice").with_rate(2)
    }

    #[test]
    fn ingress_requires_a_known_token() {
        let ingress = Ingress::new(vec![account()]);
        assert_eq!(ingress.authenticate(None), Err(Denial::MissingToken));
        assert_eq!(
            ingress.authenticate(Some("wrong")),
            Err(Denial::UnknownToken)
        );
        assert_eq!(
            ingress.authenticate(Some("sk-test")).map(|a| a.name),
            Ok("alice".to_string())
        );
    }

    #[test]
    fn ingress_without_accounts_is_open() {
        let ingress = Ingress::new(Vec::new());
        assert!(ingress.is_open());
        let account = ingress.authenticate(None).unwrap();
        assert_eq!(account.name, "open");
        assert!(ingress
            .admit(&account, MicroUsd::ZERO, Spend::default(), 0)
            .is_ok());
    }

    #[test]
    fn ingress_rate_limits_per_minute() {
        let ingress = Ingress::new(vec![account()]);
        let account = ingress.authenticate(Some("sk-test")).unwrap();
        let spent = Spend::default();
        assert!(ingress.admit(&account, MicroUsd::ZERO, spent, 100).is_ok());
        assert!(ingress.admit(&account, MicroUsd::ZERO, spent, 100).is_ok());
        assert_eq!(
            ingress.admit(&account, MicroUsd::ZERO, spent, 100),
            Err(Denial::RateLimited)
        );
        // A new minute reopens the window.
        assert!(ingress.admit(&account, MicroUsd::ZERO, spent, 101).is_ok());
    }

    #[test]
    fn ingress_refuses_an_estimate_over_the_budget() {
        let ingress = Ingress::new(vec![
            Account::new("sk-test", "alice").with_budget(MicroUsd::from_micros(100))
        ]);
        let account = ingress.authenticate(Some("sk-test")).unwrap();
        let spent = Spend {
            calls: 1,
            total: MicroUsd::from_micros(90),
        };
        assert_eq!(
            ingress.admit(&account, MicroUsd::from_micros(20), spent, 0),
            Err(Denial::OverBudget)
        );
        assert!(ingress
            .admit(&account, MicroUsd::from_micros(5), spent, 0)
            .is_ok());
    }

    #[test]
    fn ingress_parses_accounts_with_a_budget_and_rate() {
        let json = r#"{"accounts":[
            {"token":"sk-a","name":"alice","budget_usd":"0.50","requests_per_minute":60},
            {"token":"sk-b"}
        ]}"#;
        let ingress = Ingress::parse(json).unwrap();
        assert_eq!(ingress.accounts().len(), 2);
        let alice = &ingress.accounts()[0];
        assert_eq!(alice.budget, Some(MicroUsd::from_micros(500_000)));
        assert_eq!(alice.requests_per_minute, Some(60));
        assert_eq!(ingress.accounts()[1].name, "sk-b");
    }

    #[test]
    fn ingress_rejects_duplicate_tokens_and_zero_rates() {
        let duplicate = r#"{"accounts":[{"token":"a"},{"token":"a"}]}"#;
        assert!(Ingress::parse(duplicate).is_err());
        let zero = r#"{"accounts":[{"token":"a","requests_per_minute":0}]}"#;
        assert!(Ingress::parse(zero).is_err());
    }

    #[test]
    fn ingress_models_list_the_registry_and_aliases() {
        let payload = models_payload();
        let ids = payload["data"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|model| model["id"].as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"deepseek-chat"));
        assert!(ids.contains(&"anthropic/claude-3-5-sonnet") || ids.contains(&"claude-3-5-sonnet"));
        assert!(ids.contains(&"coder"));
        assert_eq!(payload["object"], "list");
    }

    #[test]
    fn ingress_reads_bearer_and_api_key_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer sk-1"),
        );
        assert_eq!(token_from(&headers), Some("sk-1"));
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("sk-2"));
        assert_eq!(token_from(&headers), Some("sk-2"));
        assert_eq!(token_from(&HeaderMap::new()), None);
    }
}
