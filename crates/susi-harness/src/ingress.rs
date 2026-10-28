//! The public ingress in front of the metering gateway.
//!
//! The gateway answers anyone who can reach its loopback port. This layer is
//! what makes it safe to expose: a bearer key names an account, the account
//! carries a money cap and a rate limit, and every admitted call is forwarded
//! to [`crate::gateway::complete`] with the account recorded as the mission —
//! so the ledger already has the per-key spend the next admission reads. The
//! account owns that mission: a caller's own `x-susi-mission` is replaced,
//! because keeping it moved the caller's rows off its account and out of the
//! cap admission reads.
//!
//! Admission is pessimistic, like the mission budget: the prompt is priced at
//! the cache-miss rate and the whole output allowance at the output rate (the
//! request's own output limit, plus the one retry the gateway makes of a
//! starved answer), so a short answer can only make the real charge smaller.
//! The estimate stays reserved until the call's ledger row is written, so
//! calls in flight together cannot each be admitted against the same
//! remaining dollar. A capped account cannot call a model the catalogue does
//! not price: the gateway books such a call at zero, and the cap would never
//! see it.
//!
//! An account with no `budget_usd` has no money cap; an ingress with no
//! accounts is open, which [`require_accounts_off_loopback`] allows on
//! loopback only.

use crate::budget::{estimate_tokens, Budget, Spend, DEFAULT_OUTPUT_RESERVE};
use crate::gateway::{complete, GatewayState};
use crate::money::MicroUsd;
use crate::providers::Registry;
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};
use susi_error::{EaiError, EaiResult};

/// The header the gateway reads a call's mission from.
const MISSION: &str = "x-susi-mission";

/// Output tokens the gateway adds to a starved answer's limit for its one
/// retry (`STARVED_HEADROOM` in [`crate::gateway`]). The retry is a second
/// billed call, so admission prices it too.
const STARVED_HEADROOM: u64 = 2_048;

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
    /// The account has a cap and the catalogue cannot price the model, so
    /// the call's spend could never count against that cap.
    Unpriced,
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
            Self::Unpriced => {
                "the model has no catalogue price, so an account with a budget cannot use it"
            }
        }
    }

    /// The HTTP status that carries the denial.
    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self {
            Self::MissingToken | Self::UnknownToken => StatusCode::UNAUTHORIZED,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::OverBudget => StatusCode::PAYMENT_REQUIRED,
            Self::Unpriced => StatusCode::BAD_REQUEST,
        }
    }
}

/// Whether `host` only accepts connections from this machine.
#[must_use]
pub fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// The serving rule every entry point shares (`susi-harness gateway`,
/// `harness-gateway` and `susi harness gateway`): an address other machines
/// can reach is served only when callers must authenticate, which takes an
/// accounts file with at least one account.
///
/// Only `susi-harness` checked this, and only for a missing file: `susi
/// harness gateway --host 0.0.0.0` served an open gateway on the operator's
/// keys, and an accounts file with an empty list passed the check and
/// admitted every caller, without a token, as the uncapped `open` account.
///
/// # Errors
///
/// A config error naming the host when it is not loopback and `ingress` is
/// missing or has no account.
pub fn require_accounts_off_loopback(host: &str, ingress: Option<&Ingress>) -> EaiResult<()> {
    if is_loopback(host) {
        return Ok(());
    }
    match ingress {
        None => Err(EaiError::config(format!(
            "--host {host} is reachable from other machines; pass --accounts with at least one \
             account so callers must authenticate"
        ))),
        Some(ingress) if ingress.is_open() => Err(EaiError::config(format!(
            "--host {host} is reachable from other machines, but the --accounts file lists no \
             account, so every caller would be admitted without a token; add an account"
        ))),
        Some(_) => Ok(()),
    }
}

/// The accounts and their admission state.
#[derive(Debug)]
pub struct Ingress {
    accounts: Vec<Account>,
    /// Per account name: its rate window and the money its calls in flight
    /// hold. One lock, so a check and its reservation are one step.
    slots: Mutex<BTreeMap<String, Slot>>,
}

#[derive(Debug, Clone, Copy)]
struct Window {
    minute: u64,
    count: u32,
}

/// One account name's admission state.
#[derive(Debug, Default)]
struct Slot {
    window: Option<Window>,
    /// The most the ledger has shown this account spending. A call that read
    /// the ledger before another call's row landed must not be admitted as
    /// if that row, and the reservation it replaced, were both absent.
    committed: MicroUsd,
    /// The estimates of the calls admitted and not yet settled.
    reserved: MicroUsd,
}

/// Money one admitted call holds against its account until it settles.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a reservation is released only by `Ingress::settle`"]
pub struct Reservation {
    name: String,
    amount: MicroUsd,
}

impl Reservation {
    /// The worst-case cost held for the call.
    #[must_use]
    pub const fn amount(&self) -> MicroUsd {
        self.amount
    }
}

impl Ingress {
    /// An ingress over the given accounts. An empty list is open.
    #[must_use]
    pub fn new(accounts: Vec<Account>) -> Self {
        Self {
            accounts,
            slots: Mutex::new(BTreeMap::new()),
        }
    }

    /// The accounts this ingress serves.
    #[must_use]
    pub fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    /// Whether the ingress admits callers without a token.
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
    /// token, a name that cannot be recorded as a mission, a non-numeric
    /// budget or a zero rate.
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

    /// Admit one call: rate window first, then the money cap, and reserve
    /// the call's estimate until [`Ingress::settle`] releases it.
    ///
    /// `estimate` is the call's worst-case cost and `spent` what the ledger
    /// already shows for this account. The check counts what the account's
    /// calls in flight have reserved: with nothing reserved, every call
    /// started before the first row landed read the same `spent`, and twenty
    /// calls that each fit were all admitted against one remaining dollar.
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
    ) -> Result<Reservation, Denial> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = slots.entry(account.name.clone()).or_default();
        if let Some(limit) = account.requests_per_minute {
            let window = slot.window.get_or_insert(Window {
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
        let Some(cap) = account.budget else {
            return Ok(Reservation {
                name: account.name.clone(),
                amount: MicroUsd::ZERO,
            });
        };
        slot.committed = slot.committed.max(spent.total);
        let committed = Spend {
            total: slot.committed,
            ..spent
        };
        if !Budget::new(cap).admits(committed, slot.reserved, estimate) {
            return Err(Denial::OverBudget);
        }
        slot.reserved = slot.reserved.saturating_add(estimate);
        Ok(Reservation {
            name: account.name.clone(),
            amount: estimate,
        })
    }

    /// Release a call's reservation once its ledger row is written, and fold
    /// in `spent`, the account's ledger total read after that.
    pub fn settle(&self, reservation: Reservation, spent: Spend) {
        if reservation.amount.is_zero() {
            return;
        }
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = slots.entry(reservation.name).or_default();
        slot.committed = slot.committed.max(spent.total);
        slot.reserved = slot
            .reserved
            .checked_sub(reservation.amount)
            .unwrap_or(MicroUsd::ZERO);
    }

    /// What the account's calls in flight hold against its cap.
    #[must_use]
    pub fn reserved(&self, name: &str) -> MicroUsd {
        let slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        slots.get(name).map_or(MicroUsd::ZERO, |slot| slot.reserved)
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
    axum::serve(listener, router(gateway, ingress))
        .with_graceful_shutdown(crate::gateway::shutdown_signal())
        .await
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
    let mut headers = headers;
    if state.ingress.is_open() {
        if !headers.contains_key(MISSION) {
            if let Some(value) = mission_header(&account.name) {
                headers.insert(MISSION, value);
            }
        }
    } else {
        // The account owns its spend. A caller's own mission header was kept,
        // so its rows landed under any name it chose: its own cap never saw
        // them, and naming another account spent that account's budget.
        let Some(value) = mission_header(&account.name) else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": {
                        "message": "the account's name cannot be recorded as a ledger mission",
                        "type": "susi_ingress",
                    }
                })),
            )
                .into_response();
        };
        headers.insert(MISSION, value);
    }
    let request = serde_json::from_str::<Value>(&body).ok();
    let (estimate, spent) = if account.budget.is_some() {
        match estimate(&state.gateway, request.as_ref(), &body) {
            Ok(estimate) => (
                estimate,
                Budget::spent(&state.gateway.ledger_path, &account.name),
            ),
            Err(denial) => return deny(denial),
        }
    } else {
        (MicroUsd::ZERO, Spend::default())
    };
    let reservation = match state.ingress.admit(&account, estimate, spent, now_minute()) {
        Ok(reservation) => reservation,
        Err(denial) => return deny(denial),
    };
    let hold = Hold {
        ingress: state.ingress.clone(),
        ledger: state.gateway.ledger_path.clone(),
        reservation: Some(reservation),
    };
    let streaming = request
        .as_ref()
        .and_then(|request| request.get("stream"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let response = complete(state.gateway.clone(), headers, body).await;
    if streaming {
        // A streamed call's row is written when its stream ends, after the
        // response has been handed back.
        hold_until_body_ends(response, hold)
    } else {
        // A buffered call's row is written before its response returns.
        drop(hold);
        response
    }
}

/// A call's reservation, released when the call is over: settled against
/// the ledger as it then stands, so the money moves from "reserved" to
/// "spent" without a moment in which it is neither.
struct Hold {
    ingress: Arc<Ingress>,
    ledger: PathBuf,
    reservation: Option<Reservation>,
}

impl Drop for Hold {
    fn drop(&mut self) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        if reservation.amount.is_zero() {
            return;
        }
        let spent = Budget::spent(&self.ledger, &reservation.name);
        self.ingress.settle(reservation, spent);
    }
}

/// `response` with `hold` kept until its body has been sent or dropped.
fn hold_until_body_ends(response: Response, hold: Hold) -> Response {
    let (parts, body) = response.into_parts();
    let chunks = futures::stream::unfold(
        (body.into_data_stream(), hold),
        |(mut chunks, hold)| async move {
            let chunk = chunks.next().await?;
            Some((chunk, (chunks, hold)))
        },
    );
    Response::from_parts(parts, Body::from_stream(chunks))
}

/// `name` as the mission header, when it reads back unchanged: the gateway
/// reads the header with `to_str`, which refuses anything but visible
/// ASCII, and books a mission it cannot read as `unscoped` — outside the
/// account's cap.
fn mission_header(name: &str) -> Option<HeaderValue> {
    let value = HeaderValue::from_str(name).ok()?;
    (value.to_str().ok() == Some(name)).then_some(value)
}

/// The worst-case cost of one call from a capped account.
///
/// # Errors
///
/// [`Denial::Unpriced`] when the catalogue cannot price the model. The
/// gateway runs such a model at a zero price, so an estimate of zero let a
/// capped account spend without limit on any name the catalogue lacks
/// (`provider/backend` spellings, or an unknown name sent to the fallback).
fn estimate(
    gateway: &GatewayState,
    request: Option<&Value>,
    body: &str,
) -> Result<MicroUsd, Denial> {
    // A body that is not JSON, or names no model, is refused by the gateway
    // before any provider is called, so it holds nothing.
    let Some(request) = request else {
        return Ok(MicroUsd::ZERO);
    };
    let Some(model) = request.get("model").and_then(Value::as_str) else {
        return Ok(MicroUsd::ZERO);
    };
    // The same lookup the gateway prices the call's row with.
    let Some(price) = gateway.catalogue.price(model) else {
        return Err(Denial::Unpriced);
    };
    let (first, retry) = output_allowance(request, gateway.max_output_tokens);
    let mut total = estimate_tokens(price, body, first);
    if let Some(retry) = retry {
        total = total.saturating_add(estimate_tokens(price, body, retry));
    }
    Ok(total)
}

/// The output tokens one call may bill, as the gateway will forward it: the
/// call's own limit, and the limit of the one retry the gateway makes when
/// the answer comes back starved (`None` when no limit can grow, so there
/// is no retry).
///
/// Admission priced a fixed 8192 output tokens whatever the request asked
/// for, so one `max_tokens: 64000` call on a strong model could bill several
/// times the headroom it was admitted against.
fn output_allowance(request: &Value, gateway_cap: Option<u32>) -> (u64, Option<u64>) {
    let cap = gateway_cap.map(u64::from);
    let limits = ["max_tokens", "max_completion_tokens"]
        .iter()
        .filter_map(|key| request.get(*key).and_then(Value::as_u64))
        .map(|asked| cap.map_or(asked, |cap| asked.min(cap)))
        .collect::<Vec<_>>();
    let Some(first) = limits.iter().copied().max() else {
        // No limit named: the gateway inserts its own cap when it has one,
        // which leaves the retry no room to grow; otherwise the provider's
        // default applies, reserved as the mission budget reserves it.
        return (cap.unwrap_or(DEFAULT_OUTPUT_RESERVE), None);
    };
    let mut grew = false;
    let mut retry = first;
    for asked in limits {
        let grown = asked.saturating_add(STARVED_HEADROOM);
        let grown = cap.map_or(grown, |cap| grown.min(cap));
        if grown > asked {
            grew = true;
            retry = retry.max(grown);
        }
    }
    (first, grew.then_some(retry))
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

/// The name an account without one is booked under: `account-` and the first
/// twelve hex digits of its token's SHA-256.
fn unnamed_account(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(token.as_bytes()));
    format!("account-{}", &digest[..12])
}

fn parse_account(raw: &Value) -> EaiResult<Account> {
    let token = raw
        .get("token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| EaiError::config("ingress account has no token"))?
        .to_string();
    // An unnamed account is named after a digest of its token, never the token:
    // the name is written to every ledger row as the mission and printed in
    // errors, and a token there is a credential anyone reading the ledger can
    // spend. The digest is stable, so the account keeps its spend across restarts.
    let name = raw
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| unnamed_account(&token), str::to_string);
    // The name is every row's mission and the key the cap is read by; one the
    // gateway cannot read back is booked as `unscoped`, outside the cap.
    if mission_header(&name).is_none() {
        return Err(EaiError::config(
            "an ingress account name must be printable ASCII: it is recorded as the mission on \
             every ledger row",
        ));
    }
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
    use crate::catalog::PriceCatalogue;
    use crate::ledger::read_entries;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// Admission read the ledger and reserved nothing, so every call started
    /// before the first row landed was checked against the same spend.
    #[test]
    fn calls_in_flight_share_the_accounts_remaining_dollars() {
        let ingress = Ingress::new(vec![
            Account::new("sk-test", "alice").with_budget(MicroUsd::from_micros(100))
        ]);
        let account = ingress.authenticate(Some("sk-test")).unwrap();
        let nothing = Spend::default();
        let sixty = MicroUsd::from_micros(60);
        let first = ingress.admit(&account, sixty, nothing, 0).unwrap();
        assert_eq!(first.amount(), sixty);
        assert_eq!(ingress.reserved("alice"), sixty);
        // The ledger still shows nothing, but sixty is spoken for.
        assert_eq!(
            ingress.admit(&account, sixty, nothing, 0),
            Err(Denial::OverBudget)
        );
        // The first call settles at 50: its reservation is released and the
        // ledger's charge counts, even against a caller that read the ledger
        // before the row landed.
        let charged = Spend {
            calls: 1,
            total: MicroUsd::from_micros(50),
        };
        ingress.settle(first, charged);
        assert_eq!(ingress.reserved("alice"), MicroUsd::ZERO);
        assert_eq!(
            ingress.admit(&account, sixty, nothing, 0),
            Err(Denial::OverBudget)
        );
        let fits = ingress
            .admit(&account, MicroUsd::from_micros(50), nothing, 0)
            .unwrap();
        ingress.settle(fits, charged);
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
        // Unnamed: booked under a digest of the token, never the token.
        assert_eq!(ingress.accounts()[1].name, unnamed_account("sk-b"));
        assert!(!ingress.accounts()[1].name.contains("sk-b"));
    }

    #[test]
    fn ingress_rejects_duplicate_tokens_and_zero_rates() {
        let duplicate = r#"{"accounts":[{"token":"a"},{"token":"a"}]}"#;
        assert!(Ingress::parse(duplicate).is_err());
        let zero = r#"{"accounts":[{"token":"a","requests_per_minute":0}]}"#;
        assert!(Ingress::parse(zero).is_err());
    }

    /// The gateway reads the mission back with `to_str`; a name it cannot
    /// read was booked as `unscoped`, so that account's cap never bound.
    #[test]
    fn an_account_name_the_gateway_cannot_read_back_is_refused() {
        let accented = r#"{"accounts":[{"token":"sk-a","name":"José","budget_usd":"1"}]}"#;
        let error = Ingress::parse(accented).unwrap_err().to_string();
        assert!(error.contains("printable ASCII"), "{error}");
        assert!(!error.contains("sk-a"), "the token never shows: {error}");
        let spaced = r#"{"accounts":[{"token":"sk-a","name":"team alpha"}]}"#;
        assert_eq!(
            Ingress::parse(spaced).unwrap().accounts()[0].name,
            "team alpha"
        );
    }

    /// An unnamed account used to be named after its token, and the name is
    /// every ledger row's mission: the ledger held a credential anyone reading
    /// it could spend. Its name is now a stable digest of the token.
    #[test]
    fn an_unnamed_account_is_never_named_after_its_token() {
        let token = "sk-ingress-secret-token-0001";
        let doc = format!(r#"{{"accounts":[{{"token":"{token}","budget_usd":"1"}}]}}"#);
        let name = Ingress::parse(&doc).unwrap().accounts()[0].name.clone();
        assert!(!name.contains(token) && !name.contains("secret"), "{name}");
        assert!(name.starts_with("account-") && name.len() == "account-".len() + 12);
        let again = Ingress::parse(&doc).unwrap().accounts()[0].name.clone();
        assert_eq!(
            name, again,
            "the same token keeps its account across restarts"
        );
        let repeated = format!(r#"{{"accounts":[{{"token":"{token}"}},{{"token":"{token}"}}]}}"#);
        let error = Ingress::parse(&repeated).unwrap_err().to_string();
        assert!(!error.contains(token), "the token never shows: {error}");
    }

    #[test]
    fn only_loopback_may_serve_without_an_account_to_authenticate() {
        for host in ["127.0.0.1", "::1", "localhost", "LOCALHOST", "127.0.0.2"] {
            assert!(is_loopback(host), "{host}");
            assert!(require_accounts_off_loopback(host, None).is_ok(), "{host}");
        }
        let empty = Ingress::parse(r#"{"accounts":[]}"#).unwrap();
        let one = Ingress::parse(r#"{"accounts":[{"token":"sk-a"}]}"#).unwrap();
        for host in ["0.0.0.0", "::", "192.168.1.10", "gateway.example"] {
            assert!(!is_loopback(host), "{host}");
            let error = require_accounts_off_loopback(host, None).unwrap_err();
            assert!(error.to_string().contains("--accounts"), "{error}");
            // An accounts file with no account admitted every caller,
            // tokenless, as the uncapped `open` account.
            let error = require_accounts_off_loopback(host, Some(&empty)).unwrap_err();
            assert!(error.to_string().contains("lists no account"), "{error}");
            assert!(require_accounts_off_loopback(host, Some(&one)).is_ok());
        }
    }

    #[test]
    fn a_requests_own_output_limit_and_the_starved_retry_are_reserved() {
        let request = |text: &str| serde_json::from_str::<Value>(text).unwrap();
        // No limit: the default reserve, or the gateway's cap that it inserts.
        assert_eq!(
            output_allowance(&request(r#"{"model":"m"}"#), None),
            (DEFAULT_OUTPUT_RESERVE, None)
        );
        assert_eq!(
            output_allowance(&request(r#"{"model":"m"}"#), Some(4_000)),
            (4_000, None)
        );
        // The request's own limit, and the retry with room the gateway makes
        // when the answer is starved.
        assert_eq!(
            output_allowance(&request(r#"{"max_tokens":64000}"#), None),
            (64_000, Some(66_048))
        );
        assert_eq!(
            output_allowance(&request(r#"{"max_completion_tokens":40}"#), Some(8_000)),
            (40, Some(2_088))
        );
        // A limit at the gateway's cap leaves the retry no room to grow.
        assert_eq!(
            output_allowance(&request(r#"{"max_tokens":64000}"#), Some(8_000)),
            (8_000, None)
        );
    }

    const CATALOGUE: &str = r#"{
        "schema": "susi/harness/prices/v1",
        "models": {
            "deepseek-flash": {"input_miss": 150000, "input_hit": 3000, "output": 600000}
        }
    }"#;

    /// The transport keeps one pooled agent per timeout, shared by every test
    /// in the process. A pooled connection to a server a finished test left
    /// behind failed calls with os error 10053; a timeout no other test uses
    /// keeps these calls off those connections, and `connection: close` on
    /// both legs keeps them from leaving any.
    const WIRE_TIMEOUT_SECS: u64 = 11;

    fn gateway_state(upstream: SocketAddr, ledger_path: PathBuf) -> Arc<GatewayState> {
        Arc::new(GatewayState {
            catalogue: PriceCatalogue::from_json(CATALOGUE).unwrap(),
            ledger_path,
            upstream_chat_url: format!("http://{upstream}/v1/chat/completions"),
            provider: "susi".into(),
            api_key: Some("upstream-test-key".into()),
            timeout_secs: WIRE_TIMEOUT_SECS,
            max_output_tokens: None,
            cache: None,
        })
    }

    #[test]
    fn a_capped_estimate_prices_the_requests_own_limit() {
        let state = gateway_state(SocketAddr::from(([127, 0, 0, 1], 9)), PathBuf::new());
        let price = state.catalogue.price("deepseek-flash").unwrap();
        let body = r#"{"model":"deepseek-flash","max_tokens":64000,"messages":[]}"#;
        let request = serde_json::from_str::<Value>(body).ok();
        let expected = estimate_tokens(price, body, 64_000)
            .saturating_add(estimate_tokens(price, body, 66_048));
        assert_eq!(estimate(&state, request.as_ref(), body), Ok(expected));
        assert!(expected > estimate_tokens(price, body, DEFAULT_OUTPUT_RESERVE));
        let unpriced = r#"{"model":"deepseek/deepseek-v4-pro","messages":[]}"#;
        let request = serde_json::from_str::<Value>(unpriced).ok();
        assert_eq!(
            estimate(&state, request.as_ref(), unpriced),
            Err(Denial::Unpriced)
        );
    }

    #[tokio::test]
    async fn a_streamed_answer_holds_its_reservation_until_the_body_ends() {
        let ingress = Arc::new(Ingress::new(vec![
            Account::new("sk-test", "alice").with_budget(MicroUsd::from_micros(100))
        ]));
        let account = ingress.authenticate(Some("sk-test")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let sixty = MicroUsd::from_micros(60);
        let hold = |reservation| Hold {
            ingress: ingress.clone(),
            ledger: dir.path().join("ledger.jsonl"),
            reservation: Some(reservation),
        };
        let streamed = |chunks: Vec<&'static str>| {
            Response::new(Body::from_stream(futures::stream::iter(
                chunks.into_iter().map(Ok::<_, std::io::Error>),
            )))
        };

        let reservation = ingress.admit(&account, sixty, Spend::default(), 0).unwrap();
        let response = hold_until_body_ends(
            streamed(vec!["data: a\n\n", "data: b\n\n"]),
            hold(reservation),
        );
        assert_eq!(ingress.reserved("alice"), sixty, "held while streaming");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"data: a\n\ndata: b\n\n");
        assert_eq!(
            ingress.reserved("alice"),
            MicroUsd::ZERO,
            "released at the end"
        );

        // A caller that hangs up mid-stream releases it too.
        let reservation = ingress.admit(&account, sixty, Spend::default(), 0).unwrap();
        let response = hold_until_body_ends(streamed(vec!["data: a\n\n"]), hold(reservation));
        drop(response);
        assert_eq!(ingress.reserved("alice"), MicroUsd::ZERO);
    }

    async fn spawn(app: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        address
    }

    /// POST `body` to the ingress at `address` with `token` and `extra` headers.
    async fn send(
        address: SocketAddr,
        token: &str,
        extra: &[(&str, &str)],
        body: &str,
    ) -> (u16, String) {
        let url = format!("http://{address}/v1/chat/completions");
        let body = body.to_string();
        let mut headers = vec![
            ("authorization".to_string(), format!("Bearer {token}")),
            ("connection".to_string(), "close".to_string()),
        ];
        headers.extend(
            extra
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string())),
        );
        tokio::task::spawn_blocking(move || {
            let forwarded = headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect::<Vec<_>>();
            let call = susi_http_transport::http_call_with_body(
                "POST",
                &url,
                &forwarded,
                Some(body.as_bytes()),
                WIRE_TIMEOUT_SECS,
                0,
            )
            .unwrap();
            let status = call.status;
            (status, call.into_utf8(1024 * 1024).unwrap())
        })
        .await
        .unwrap()
    }

    /// The upstream's answer, on a connection it closes after answering.
    fn answer() -> impl IntoResponse {
        (
            [(header::CONNECTION, "close")],
            Json(json!({
                "id": "cmpl-ingress",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 5}
            })),
        )
    }

    /// An upstream that answers every call and counts them.
    async fn counting_upstream() -> (SocketAddr, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                let counted = counted.clone();
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    answer()
                }
            }),
        );
        (spawn(app).await, calls)
    }

    const BODY: &str =
        r#"{"model":"deepseek-flash","messages":[{"role":"user","content":"ping"}]}"#;

    #[tokio::test]
    async fn a_caller_cannot_move_its_spend_off_its_account_with_its_own_mission() {
        let (upstream, _) = counting_upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let ingress = Arc::new(Ingress::new(vec![
            Account::new("sk-alice", "alice").with_budget(MicroUsd::from_micros(1_000_000)),
            Account::new("sk-bob", "bob").with_budget(MicroUsd::from_micros(1_000_000)),
        ]));
        let proxy = spawn(router(gateway_state(upstream, ledger.clone()), ingress)).await;

        for mission in ["bob", "anything"] {
            let (status, text) = send(proxy, "sk-alice", &[(MISSION, mission)], BODY).await;
            assert_eq!(status, 200, "{text}");
        }
        let entries = read_entries(&ledger).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries.iter().all(|entry| entry.mission == "alice"),
            "{:?}",
            entries
                .iter()
                .map(|entry| &entry.mission)
                .collect::<Vec<_>>()
        );
        assert_eq!(Budget::spent(&ledger, "bob").calls, 0);
    }

    #[tokio::test]
    async fn a_capped_account_cannot_call_a_model_the_catalogue_does_not_price() {
        let (upstream, calls) = counting_upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let ingress = Arc::new(Ingress::new(vec![
            Account::new("sk-capped", "capped").with_budget(MicroUsd::from_micros(2_000_000)),
            Account::new("sk-open", "uncapped"),
        ]));
        let proxy = spawn(router(gateway_state(upstream, ledger.clone()), ingress)).await;
        let unpriced = r#"{"model":"deepseek/deepseek-v4-pro","messages":[]}"#;

        let (status, text) = send(proxy, "sk-capped", &[], unpriced).await;
        assert_eq!(status, 400, "{text}");
        assert!(text.contains("no catalogue price"), "{text}");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "refused before dispatch");
        assert!(!ledger.exists());

        // With no cap there is nothing to bypass: the call runs, uncosted.
        let (status, text) = send(proxy, "sk-open", &[], unpriced).await;
        assert_eq!(status, 200, "{text}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn calls_in_flight_hold_their_estimate_so_together_they_cannot_overrun_the_cap() {
        let arrived = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let app = {
            let (arrived, gate) = (arrived.clone(), gate.clone());
            Router::new().route(
                "/v1/chat/completions",
                post(move || {
                    let (arrived, gate) = (arrived.clone(), gate.clone());
                    async move {
                        arrived.notify_one();
                        let _open = gate.acquire().await;
                        answer()
                    }
                }),
            )
        };
        let upstream = spawn(app).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let state = gateway_state(upstream, ledger.clone());
        let price = state.catalogue.price("deepseek-flash").unwrap();
        // Room for one call's worst case, not two.
        let one = estimate_tokens(price, BODY, DEFAULT_OUTPUT_RESERVE);
        let cap = MicroUsd::from_micros(one.micros().saturating_mul(3) / 2);
        let ingress = Arc::new(Ingress::new(vec![
            Account::new("sk-alice", "alice").with_budget(cap)
        ]));
        let proxy = spawn(router(state, ingress.clone())).await;

        let first = tokio::spawn(async move { send(proxy, "sk-alice", &[], BODY).await });
        arrived.notified().await;
        assert_eq!(ingress.reserved("alice"), one);
        // The ledger still shows nothing spent, but the first call's worst
        // case is spoken for.
        let (status, text) = send(proxy, "sk-alice", &[], BODY).await;
        assert_eq!(status, 402, "{text}");

        gate.add_permits(1);
        let (status, text) = first.await.unwrap();
        assert_eq!(status, 200, "{text}");
        assert_eq!(ingress.reserved("alice"), MicroUsd::ZERO);
        // Settled at its real, small cost, the room is back.
        let (status, text) = send(proxy, "sk-alice", &[], BODY).await;
        assert_eq!(status, 200, "{text}");
        assert_eq!(read_entries(&ledger).unwrap().len(), 2);
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
        assert!(ids.contains(&"deepseek-flash"));
        assert!(ids.contains(&"anthropic/claude-sonnet") || ids.contains(&"claude-sonnet"));
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
