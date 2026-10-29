//! The public ingress in front of the metering gateway.
//!
//! The gateway answers anyone who can reach its loopback port. This layer is
//! what makes it safe to expose: a bearer key names an account, the account
//! carries a money cap and a rate limit, and every admitted call is forwarded
//! to [`crate::gateway::complete_holding`] with the account recorded as the
//! mission — so the ledger already has the per-key spend the next admission
//! reads. The account owns that mission: a caller's own `x-susi-mission` is
//! replaced, because keeping it moved the caller's rows off its account and
//! out of the cap admission reads.
//!
//! Admission is pessimistic, like the mission budget: the prompt is priced at
//! the cache-miss rate and the whole output allowance at the output rate (the
//! request's own output limit, plus the one retry the gateway makes of a
//! starved answer, for each of the `n` answers it asks for). A capped call
//! goes out asking for what admission priced and nothing more: its output
//! limit is sent as the one admission priced, under the one name
//! `max_tokens` that every OpenAI-compatible server reads (the gateway sends
//! it again as `max_completion_tokens` to a provider that refuses
//! `max_tokens`), one that names no output limit is sent with the one
//! admission priced for it, and a limit or an `n` that is not a whole number
//! is replaced by the value priced, so the provider cannot answer at more
//! length, or more often, than was reserved, and a short answer can only
//! make the real charge smaller. The estimate stays reserved
//! until the call's ledger row is written, even when the caller hangs up
//! while the provider is still answering, so calls in flight together cannot
//! each be admitted against the same remaining dollar. A capped account
//! cannot call a model the catalogue does not price: the gateway books such a
//! call at zero, and the cap would never see it.
//!
//! An account with no `budget_usd` has no money cap; an ingress with no
//! accounts is open, which [`require_accounts_off_loopback`] allows on
//! loopback only.

use crate::budget::{estimate_tokens, Budget, Spend, DEFAULT_OUTPUT_RESERVE};
use crate::gateway::{complete_holding, GatewayState};
use crate::money::MicroUsd;
use crate::providers::Registry;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
    /// The accounts file gave no name, so earlier releases, which named such
    /// an account after its token, booked its rows under the token itself
    /// ([`Account::spent`] reads them; nothing writes them any more).
    booked_under_token: bool,
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
            booked_under_token: false,
        }
    }

    /// What the ledger shows this account spending: the rows booked under its
    /// name and, for an account the accounts file names nowhere, the rows
    /// earlier releases booked under its token.
    ///
    /// Such an account is now booked under a digest of its token, so the
    /// token never reaches the ledger again. Read by that name alone, its
    /// spend began again at zero on the upgrade, and an account that had used
    /// most of its cap could spend all of it once more. The token is only
    /// compared with the missions already written; it is never written or
    /// printed.
    #[must_use]
    pub fn spent(&self, ledger: &Path) -> Spend {
        if self.booked_under_token {
            Budget::spent_any(ledger, &[&self.name, &self.token])
        } else {
            Budget::spent(ledger, &self.name)
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
        Self::new("", "open")
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
    /// The account has a cap and the call sends a file or a video by
    /// reference, whose cost nothing before the call can bound.
    UnboundedInput,
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
            Self::UnboundedInput => {
                "an account with a budget cannot send a file or a video by reference: its cost \
                 cannot be bounded before the call; send it inline"
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
            Self::Unpriced | Self::UnboundedInput => StatusCode::BAD_REQUEST,
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
    let mut body = body;
    let (estimate, spent) = if account.budget.is_some() {
        let mut request = serde_json::from_str::<Value>(&body).ok();
        if let Some(request) = request.as_mut() {
            if send_as_priced(request, state.gateway.max_output_tokens) {
                body = request.to_string();
            }
        }
        match estimate(&state.gateway, request.as_ref(), &body) {
            Ok(estimate) => (estimate, account.spent(&state.gateway.ledger_path)),
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
        account,
        reservation: Some(reservation),
    };
    complete_holding(state.gateway.clone(), headers, body, Some(Box::new(hold))).await
}

/// A call's reservation, released when the gateway has settled the call:
/// [`complete_holding`] drops it once the call's ledger row is written (or
/// once it is clear none will be), and it is settled against the ledger as
/// it then stands, so the money moves from "reserved" to "spent" without a
/// moment in which it is neither.
///
/// It was dropped with the request, and the gateway goes on with a call
/// whose client has hung up: a caller that closed each socket once its
/// request was sent got its reservation back at once while the provider
/// went on answering and billing, and its next call was admitted against
/// the same remaining dollar.
struct Hold {
    ingress: Arc<Ingress>,
    ledger: PathBuf,
    account: Account,
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
        let spent = self.account.spent(&self.ledger);
        self.ingress.settle(reservation, spent);
    }
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
    // What the provider fetches is billed but not in the body: an image URL is
    // about 80 bytes, priced as 20 tokens, and billed as up to a thousand or
    // more. 300 of them in one capped call billed 45 times its reservation.
    let fetched = price.cost(crate::pricing::TokenUsage::new(
        fetched_input_tokens(request)?,
        0,
        0,
    ));
    let mut total = estimate_tokens(price, body, first).saturating_add(fetched);
    if let Some(retry) = retry {
        total = total
            .saturating_add(estimate_tokens(price, body, retry))
            .saturating_add(fetched);
    }
    // Every answer bills its own output, and the prompt is priced again for
    // each in case a provider bills it per answer: priced as one, a call
    // asking for 64 answers billed up to 64 times its reservation.
    Ok(total.saturating_mul(answers(request)))
}

/// Input tokens one image the provider fetches by URL is priced at: above
/// what any routed provider bills for a single image at its highest detail,
/// so the reservation covers it whatever its size.
const FETCHED_IMAGE_TOKENS: u64 = 6_000;

/// Input a capped call makes the provider fetch, which the body's length
/// does not show: each image by URL at [`FETCHED_IMAGE_TOKENS`]. A file or a
/// video by reference has no ceiling at all, so it is refused. Inline data
/// (`data:` URLs, base64 sources, `file_data`) is in the body and priced by
/// its length.
fn fetched_input_tokens(request: &Value) -> Result<u64, Denial> {
    let parts = request
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| message.get("content").and_then(Value::as_array))
        .flatten();
    let mut tokens: u64 = 0;
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("image_url") => {
                let url = part
                    .get("image_url")
                    .and_then(|image| image.as_str().or_else(|| image.get("url")?.as_str()))
                    .unwrap_or_default();
                if !url.trim_start().starts_with("data:") {
                    tokens = tokens.saturating_add(FETCHED_IMAGE_TOKENS);
                }
            }
            Some("image") => {
                let inline = part
                    .get("source")
                    .and_then(|source| source.get("type"))
                    .and_then(Value::as_str)
                    == Some("base64");
                if !inline {
                    tokens = tokens.saturating_add(FETCHED_IMAGE_TOKENS);
                }
            }
            Some("file" | "input_file") => {
                let file = part.get("file").unwrap_or(part);
                if file.get("file_data").is_none() {
                    return Err(Denial::UnboundedInput);
                }
            }
            Some("document") => {
                let inline = part
                    .get("source")
                    .and_then(|source| source.get("type"))
                    .and_then(Value::as_str)
                    == Some("base64");
                if !inline {
                    return Err(Denial::UnboundedInput);
                }
            }
            Some("video_url" | "video" | "input_video") => {
                return Err(Denial::UnboundedInput);
            }
            _ => {}
        }
    }
    Ok(tokens)
}

/// How many answers a call asks for (`n`, one when it names none). A count
/// written as a float (`2.5`) or as a string (`"8"`) is read as a lenient
/// provider reads it, rounded up; the cast saturates.
fn answers(request: &Value) -> u64 {
    request
        .get("n")
        .and_then(|n| {
            n.as_u64().or_else(|| {
                n.as_f64()
                    .or_else(|| n.as_str().and_then(|text| text.trim().parse::<f64>().ok()))
                    .filter(|n| n.is_finite() && *n >= 1.0)
                    .map(|n| n.ceil() as u64)
            })
        })
        .unwrap_or(1)
        .max(1)
}

/// Make a capped call ask the provider for no more than admission prices:
/// its output limit ([`name_output_limit`]) and its count of answers
/// ([`name_answers`]). Returns whether the request changed, so the body that
/// goes out is written again.
fn send_as_priced(request: &mut Value, gateway_cap: Option<u32>) -> bool {
    // `|`, not `||`: both run, whatever the first one changed.
    name_output_limit(request, gateway_cap) | name_answers(request)
}

/// The names a request gives its output limit.
const OUTPUT_LIMITS: [&str; 2] = ["max_tokens", "max_completion_tokens"];

/// A value under one of [`OUTPUT_LIMITS`], as a limit admission can price:
/// a whole number of tokens above zero. Anything else there (`null`, `0`,
/// `-1`, `50000.0`, `"8"`) is read by some provider as no limit at all, and
/// by a lenient one as a number admission never saw.
fn output_limit(value: &Value) -> Option<u64> {
    value.as_u64().filter(|limit| *limit > 0)
}

/// Send a capped call's output limit as the one admission prices, under the
/// one name `max_tokens`: the larger [`output_limit`] the request names
/// under either of [`OUTPUT_LIMITS`] ([`output_allowance`] prices that one),
/// else the one admission prices for a call that names none (the gateway's
/// `--max-output-tokens`, else [`DEFAULT_OUTPUT_RESERVE`]). Nothing else is
/// left under either name. The gateway then clamps it to its own
/// `--max-output-tokens`, as admission did. Returns whether the request
/// changed.
///
/// Admission reserved 8192 output tokens for a call that named no limit,
/// but the call went out without one, and an OpenAI- or Gemini-compatible
/// provider then applies the model's own maximum: one call could bill many
/// times what it was admitted against. A lone `max_completion_tokens` went
/// out as it was, priced, but a server that reads only `max_tokens`
/// (DeepSeek, older vLLM and llama.cpp) ignored it and applied its own
/// default. `max_tokens` is the name every OpenAI-compatible server reads,
/// and the one the gateway's own cap names ([`crate::gateway`]); a provider
/// that refuses it for `max_completion_tokens` (OpenAI's reasoning models)
/// is sent the call again by the gateway with the same limit under that name.
///
/// A value that is not a limit was kept whenever the other name held one, so
/// `{"max_tokens":100,"max_completion_tokens":50000.0}` was priced at 100
/// tokens and went out as it was: a vLLM-style server reads `50000.0` as a
/// whole number and prefers `max_completion_tokens`, and billed 50000.
fn name_output_limit(request: &mut Value, gateway_cap: Option<u32>) -> bool {
    let Some(object) = request.as_object_mut() else {
        return false;
    };
    let limit = OUTPUT_LIMITS
        .iter()
        .filter_map(|key| object.get(*key).and_then(output_limit))
        .max()
        .unwrap_or_else(|| gateway_cap.map_or(DEFAULT_OUTPUT_RESERVE, u64::from));
    let limit = Value::from(limit);
    let changed =
        object.get("max_tokens") != Some(&limit) || object.contains_key("max_completion_tokens");
    object.remove("max_completion_tokens");
    object.insert("max_tokens".to_string(), limit);
    changed
}

/// Make a capped call's `n` the count admission prices ([`answers`]) when it
/// is not a whole number. Returns whether the request changed.
///
/// A count admission could not read was priced as one answer and went out
/// as it was: a provider that reads `"8"` as a number (a proto3-JSON front
/// end, or pydantic's lax mode in a vLLM or LiteLLM server behind
/// `--upstream`) answered eight times, billed against an estimate for one.
/// A whole-number `n` is the caller's and is left as it is.
fn name_answers(request: &mut Value) -> bool {
    let count = answers(request);
    let Some(object) = request.as_object_mut() else {
        return false;
    };
    if object.get("n").is_none_or(Value::is_u64) {
        return false;
    }
    object.insert("n".to_string(), Value::from(count));
    true
}

/// The output tokens one call may bill, as the gateway will forward it: the
/// call's own limit, and the limit of the one retry the gateway makes when
/// the answer comes back starved (`None` when no limit can grow, so there
/// is no retry). A capped call names one limit by now, as `max_tokens`
/// ([`name_output_limit`]); a request that still names none is priced at the
/// limit that call would have been sent with, and one that names two at the
/// larger, which is the one it would be sent with.
///
/// Admission priced a fixed 8192 output tokens whatever the request asked
/// for, so one `max_tokens: 64000` call on a strong model could bill several
/// times the headroom it was admitted against.
fn output_allowance(request: &Value, gateway_cap: Option<u32>) -> (u64, Option<u64>) {
    let cap = gateway_cap.map(u64::from);
    let mut limits = OUTPUT_LIMITS
        .iter()
        .filter_map(|key| request.get(*key).and_then(output_limit))
        .map(|asked| cap.map_or(asked, |cap| asked.min(cap)))
        .collect::<Vec<_>>();
    if limits.is_empty() {
        limits.push(cap.unwrap_or(DEFAULT_OUTPUT_RESERVE));
    }
    let first = limits.iter().copied().max().unwrap_or(0);
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
    // spend. The digest is stable, so the account keeps its spend across
    // restarts, and the rows earlier releases booked under the token itself
    // still count against its cap (`Account::spent`).
    let given = raw
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let booked_under_token = given.is_none();
    let name = given.map_or_else(|| unnamed_account(&token), str::to_string);
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
        booked_under_token,
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
        // No limit: the one named for it, the default reserve or the
        // gateway's cap. The default can grow on the starved retry; the cap
        // cannot.
        assert_eq!(
            output_allowance(&request(r#"{"model":"m"}"#), None),
            (DEFAULT_OUTPUT_RESERVE, Some(DEFAULT_OUTPUT_RESERVE + 2_048))
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

    /// A capped call that named no output limit was priced at 8192 output
    /// tokens and sent with none, so the provider's own default (often the
    /// model's maximum) applied. It now goes out with the limit admission
    /// priced; a limit the caller named goes out at the value it named,
    /// under the one name every server reads.
    #[test]
    fn a_capped_call_is_sent_with_the_output_limit_admission_priced() {
        let named = |text: &str, cap: Option<u32>| {
            let mut request = serde_json::from_str::<Value>(text).unwrap();
            let changed = name_output_limit(&mut request, cap);
            (changed, request)
        };
        let (changed, request) = named(r#"{"model":"m"}"#, None);
        assert!(changed);
        assert_eq!(request["max_tokens"], DEFAULT_OUTPUT_RESERVE);
        let (_, request) = named(r#"{"model":"m"}"#, Some(4_000));
        assert_eq!(request["max_tokens"], 4_000);
        // Either name is the caller's limit; it goes out as `max_tokens`,
        // which a server that reads only that name does not ignore.
        let (changed, request) = named(r#"{"model":"m","max_completion_tokens":40}"#, None);
        assert!(changed);
        assert_eq!(request, json!({"model": "m", "max_tokens": 40}));
        let (changed, _) = named(r#"{"model":"m","max_tokens":64000}"#, None);
        assert!(!changed);
        // Two limits go out as the larger, the one admission priced; the
        // gateway clamps it to its own cap, as admission did.
        for (tokens, completion) in [(100, 5_000), (5_000, 100)] {
            let (changed, request) = named(
                &format!(
                    r#"{{"model":"m","max_tokens":{tokens},"max_completion_tokens":{completion}}}"#
                ),
                Some(4_000),
            );
            assert!(changed);
            assert_eq!(request, json!({"model": "m", "max_tokens": 5_000}));
            assert_eq!(output_allowance(&request, Some(4_000)), (4_000, None));
        }
        // A limit that is not a whole number reads as none to some providers.
        let (changed, request) = named(r#"{"model":"m","max_completion_tokens":null}"#, None);
        assert!(changed);
        assert_eq!(
            request,
            json!({"model": "m", "max_tokens": DEFAULT_OUTPUT_RESERVE})
        );
        let (_, request) = named(r#"{"model":"m","max_tokens":"lots"}"#, None);
        assert_eq!(request["max_tokens"], DEFAULT_OUTPUT_RESERVE);
        let (_, request) = named(r#"{"model":"m","max_tokens":0}"#, None);
        assert_eq!(request["max_tokens"], DEFAULT_OUTPUT_RESERVE);
    }

    /// A value under a limit's name that is not a whole number was kept
    /// whenever the other name held one: priced at the one, it went out
    /// beside it, and a server that reads `50000.0` as a number and prefers
    /// `max_completion_tokens` billed 50000 tokens on a 100-token admission.
    #[test]
    fn a_limit_that_is_not_a_whole_number_never_goes_out_beside_one_that_is() {
        let named = |text: &str| {
            let mut request = serde_json::from_str::<Value>(text).unwrap();
            let changed = name_output_limit(&mut request, None);
            (changed, request)
        };
        let (changed, request) =
            named(r#"{"model":"m","max_tokens":100,"max_completion_tokens":50000.0}"#);
        assert!(changed, "written again without the value");
        assert_eq!(request, json!({"model": "m", "max_tokens": 100}));
        assert_eq!(output_allowance(&request, None), (100, Some(2_148)));
        for other in ["-1", "\"50000\"", "null", "0"] {
            let (changed, request) = named(&format!(
                r#"{{"model":"m","max_tokens":{other},"max_completion_tokens":40}}"#
            ));
            assert!(changed, "{other}");
            assert_eq!(request, json!({"model": "m", "max_tokens": 40}), "{other}");
            assert_eq!(output_allowance(&request, None), (40, Some(2_088)));
        }
    }

    /// An `n` that is not a whole number was priced as admission read it and
    /// went out as it was: `"8"` was priced as one answer, and a provider
    /// that reads numeric strings as numbers answered eight times.
    #[test]
    fn a_count_of_answers_that_is_not_a_whole_number_goes_out_as_the_one_priced() {
        let state = gateway_state(SocketAddr::from(([127, 0, 0, 1], 9)), PathBuf::new());
        let body = r#"{"model":"deepseek-flash","max_tokens":1000,"n":"8","messages":[]}"#;
        let mut eight = serde_json::from_str::<Value>(body).unwrap();
        assert!(send_as_priced(&mut eight, None));
        assert_eq!(eight["n"], 8);
        assert!(eight["n"].is_u64(), "{eight}");
        let mut one = eight.clone();
        one.as_object_mut().unwrap().remove("n");
        let single = estimate(&state, Some(&one), body).unwrap();
        assert_eq!(
            estimate(&state, Some(&eight), body),
            Ok(single.saturating_mul(8))
        );

        let sent = |text: &str| {
            let mut request = serde_json::from_str::<Value>(text).unwrap();
            let changed = name_answers(&mut request);
            (changed, request.get("n").cloned())
        };
        assert_eq!(sent(r#"{"n":2.5}"#), (true, Some(json!(3))));
        assert_eq!(sent(r#"{"n":"lots"}"#), (true, Some(json!(1))));
        assert_eq!(sent(r#"{"n":null}"#), (true, Some(json!(1))));
        // A whole number is the caller's, and a request without one gets none.
        assert_eq!(sent(r#"{"n":4}"#), (false, Some(json!(4))));
        assert_eq!(sent(r#"{"n":0}"#), (false, Some(json!(0))));
        assert_eq!(sent(r#"{}"#), (false, None));
    }

    /// `n` asks for that many answers, each billed: priced as one, a call
    /// could bill many times what it reserved.
    #[test]
    fn every_answer_a_call_asks_for_is_priced() {
        let state = gateway_state(SocketAddr::from(([127, 0, 0, 1], 9)), PathBuf::new());
        let body = r#"{"model":"deepseek-flash","max_tokens":1000,"n":4,"messages":[]}"#;
        let four = serde_json::from_str::<Value>(body).unwrap();
        let mut one = four.clone();
        one.as_object_mut().unwrap().remove("n");
        let single = estimate(&state, Some(&one), body).unwrap();
        assert!(single > MicroUsd::ZERO);
        assert_eq!(
            estimate(&state, Some(&four), body),
            Ok(single.saturating_mul(4))
        );
        let answers_of = |text: &str| answers(&serde_json::from_str::<Value>(text).unwrap());
        assert_eq!(answers_of(r#"{}"#), 1);
        assert_eq!(answers_of(r#"{"n":null}"#), 1);
        assert_eq!(answers_of(r#"{"n":0}"#), 1);
        assert_eq!(answers_of(r#"{"n":64}"#), 64);
        assert_eq!(answers_of(r#"{"n":2.5}"#), 3);
        assert_eq!(answers_of(r#"{"n":"8"}"#), 8);
        assert_eq!(answers_of(r#"{"n":"lots"}"#), 1);
    }

    /// What a provider fetches is billed but is not in the body: priced by
    /// the body's length, 300 image URLs in one capped call billed 45 times
    /// its reservation. Each fetched image is priced at a ceiling; a file or a
    /// video by reference has none and is refused; inline data is priced by
    /// its length as before.
    #[test]
    fn what_the_provider_fetches_is_priced_or_refused() {
        let state = gateway_state(SocketAddr::from(([127, 0, 0, 1], 9)), PathBuf::new());
        let price = state.catalogue.price("deepseek-flash").unwrap();
        let with = |content: &str| {
            let body = format!(
                r#"{{"model":"deepseek-flash","max_tokens":10,"messages":[{{"role":"user","content":[{content}]}}]}}"#
            );
            let request = serde_json::from_str::<Value>(&body).unwrap();
            (estimate(&state, Some(&request), &body), body)
        };
        let fetched_of = |content: &str| {
            let body = format!(r#"{{"messages":[{{"role":"user","content":[{content}]}}]}}"#);
            fetched_input_tokens(&serde_json::from_str::<Value>(&body).unwrap())
        };
        let remote_image = r#"{"type":"image_url","image_url":{"url":"https://example.test/big.png","detail":"high"}}"#;
        assert_eq!(fetched_of(remote_image), Ok(FETCHED_IMAGE_TOKENS));
        assert_eq!(
            fetched_of(&[remote_image; 3].join(",")),
            Ok(3 * FETCHED_IMAGE_TOKENS),
            "every fetched image is priced"
        );
        assert_eq!(
            fetched_of(
                r#"{"type":"image","source":{"type":"url","url":"https://example.test/a.png"}}"#
            ),
            Ok(FETCHED_IMAGE_TOKENS)
        );
        assert_eq!(
            fetched_of(r#"{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}"#),
            Ok(0),
            "inline data is in the body and priced by its length"
        );
        let (text, _) = with(r#"{"type":"text","text":"hi"}"#);
        let (remote, _) = with(&format!(r#"{{"type":"text","text":"hi"}},{remote_image}"#));
        let fetched = price.cost(crate::pricing::TokenUsage::new(FETCHED_IMAGE_TOKENS, 0, 0));
        assert!(
            remote.unwrap().micros() >= text.unwrap().micros() + fetched.micros(),
            "the call's reservation carries the fetched image's ceiling"
        );
        for unbounded in [
            r#"{"type":"file","file":{"file_id":"file-abc"}}"#,
            r#"{"type":"video_url","video_url":{"url":"https://example.test/v.mp4"}}"#,
            r#"{"type":"document","source":{"type":"url","url":"https://example.test/a.pdf"}}"#,
        ] {
            assert_eq!(
                with(unbounded).0,
                Err(Denial::UnboundedInput),
                "{unbounded}"
            );
        }
        let (inline_file, _) =
            with(r#"{"type":"file","file":{"file_data":"data:application/pdf;base64,AAAA"}}"#);
        assert!(inline_file.is_ok());
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

    /// What admission reserves for `body` from a capped account, priced as
    /// `completions` prices it.
    fn reserved_for(state: &GatewayState, body: &str) -> MicroUsd {
        let mut request = serde_json::from_str::<Value>(body).unwrap();
        let body = if send_as_priced(&mut request, state.max_output_tokens) {
            request.to_string()
        } else {
            body.to_string()
        };
        estimate(state, Some(&request), &body).unwrap()
    }

    /// Serve `app` for one test, reading every request in full first: a
    /// connection closed (these servers answer `connection: close`) with part
    /// of a request unread is reset on Windows, and the gateway's call to it
    /// failed with os error 10054.
    async fn spawn(app: Router) -> SocketAddr {
        let app = app.layer(axum::middleware::map_request(
            |request: axum::extract::Request| async move {
                let (parts, body) = request.into_parts();
                let bytes = axum::body::to_bytes(body, usize::MAX)
                    .await
                    .unwrap_or_default();
                axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes))
            },
        ));
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
        // Room for one call's worst case, not two.
        let one = reserved_for(&state, BODY);
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

    const STREAMED: &str =
        r#"{"model":"deepseek-flash","stream":true,"messages":[{"role":"user","content":"ping"}]}"#;

    /// An upstream that says when a call arrives and answers only once the
    /// gate lets it: buffered, or as a stream whose first chunk comes at once
    /// and whose usage comes after the gate.
    async fn gated_upstream() -> (
        SocketAddr,
        Arc<tokio::sync::Notify>,
        Arc<tokio::sync::Semaphore>,
    ) {
        let arrived = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let app = {
            let (arrived, gate) = (arrived.clone(), gate.clone());
            Router::new().route(
                "/v1/chat/completions",
                post(move |body: String| {
                    let (arrived, gate) = (arrived.clone(), gate.clone());
                    async move {
                        arrived.notify_one();
                        if !body.contains(r#""stream":true"#) {
                            let _open = gate.acquire().await;
                            return answer().into_response();
                        }
                        let (tx, mut rx) =
                            tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(4);
                        tokio::spawn(async move {
                            let first = "data: {\"id\":\"cmpl-s\",\"choices\":[{\"delta\":{\"content\":\"PARTIAL\"}}]}\n\n";
                            let _ = tx.send(Ok(first.to_string())).await;
                            let _open = gate.acquire().await;
                            let rest = concat!(
                                "data: {\"id\":\"cmpl-s\",\"choices\":[],\"usage\":",
                                "{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
                                "data: [DONE]\n\n"
                            );
                            let _ = tx.send(Ok(rest.to_string())).await;
                        });
                        let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx));
                        (
                            [
                                (header::CONTENT_TYPE, "text/event-stream"),
                                (header::CONNECTION, "close"),
                            ],
                            axum::body::Body::from_stream(stream),
                        )
                            .into_response()
                    }
                }),
            )
        };
        (spawn(app).await, arrived, gate)
    }

    /// POST `body` with `token` over a raw socket and hang up: once the
    /// upstream has the call when `wait_for` is empty, otherwise as soon as
    /// the answer shows `wait_for`. A client timeout, Esc in an agent, or a
    /// caller closing each socket on purpose, as the ingress sees it.
    async fn post_and_hang_up(
        address: SocketAddr,
        token: &str,
        body: &str,
        wait_for: &str,
        arrived: &tokio::sync::Notify,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nhost: {address}\r\n\
             authorization: Bearer {token}\r\ncontent-type: application/json\r\n\
             content-length: {}\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        if wait_for.is_empty() {
            arrived.notified().await;
        } else {
            let mut seen = Vec::new();
            let mut buffer = [0u8; 1024];
            while !String::from_utf8_lossy(&seen).contains(wait_for) {
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0, "the ingress closed before `{wait_for}`");
                seen.extend_from_slice(&buffer[..read]);
            }
        }
        drop(socket);
    }

    /// `check` once it holds, or `false` after five seconds.
    async fn eventually(check: impl Fn() -> bool) -> bool {
        for _ in 0..100 {
            if check() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        check()
    }

    /// A caller that hangs up while the provider is still answering keeps
    /// its reservation until the call's row is written. Released with the
    /// request, it came back the moment the client went, while the gateway
    /// went on with the call and billed it: a caller that closed each socket
    /// once its request was sent had every call admitted against the same
    /// remaining dollar.
    #[tokio::test]
    async fn a_caller_that_hangs_up_keeps_its_reservation_until_the_call_is_settled() {
        for (body, wait_for) in [(BODY, ""), (STREAMED, "PARTIAL")] {
            let (upstream, arrived, gate) = gated_upstream().await;
            let dir = tempfile::tempdir().unwrap();
            let ledger = dir.path().join("ledger.jsonl");
            let state = gateway_state(upstream, ledger.clone());
            // Room for one call's worst case, not two.
            let one = reserved_for(&state, body);
            let cap = MicroUsd::from_micros(one.micros().saturating_mul(3) / 2);
            let ingress = Arc::new(Ingress::new(vec![
                Account::new("sk-alice", "alice").with_budget(cap)
            ]));
            let proxy = spawn(router(state, ingress.clone())).await;

            post_and_hang_up(proxy, "sk-alice", body, wait_for, &arrived).await;
            // Time for the server to see the hang-up and drop the request.
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            assert_eq!(
                ingress.reserved("alice"),
                one,
                "held while the provider answers: {body}"
            );
            let (status, text) = send(proxy, "sk-alice", &[], body).await;
            assert_eq!(status, 402, "{body}: {text}");

            gate.add_permits(1);
            assert!(
                eventually(|| ingress.reserved("alice") == MicroUsd::ZERO).await,
                "released once the call is settled: {body}"
            );
            let (rows, _) = crate::ledger::read_entries_lenient(&ledger);
            assert_eq!(rows.len(), 1, "{body}");
            assert_eq!(rows[0].mission, "alice");
        }
    }

    /// An upstream that answers every call and keeps each request it got.
    async fn recording_upstream() -> (SocketAddr, Arc<Mutex<Vec<Value>>>) {
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = {
            let seen = seen.clone();
            Router::new().route(
                "/v1/chat/completions",
                post(move |body: String| {
                    let seen = seen.clone();
                    async move {
                        seen.lock()
                            .unwrap()
                            .push(serde_json::from_str(&body).unwrap());
                        answer()
                    }
                }),
            )
        };
        (spawn(app).await, seen)
    }

    /// A capped call that named no output limit was admitted against 8192
    /// output tokens and sent with none, so the provider's own default, often
    /// the model's maximum, decided what it billed.
    #[tokio::test]
    async fn a_capped_call_that_names_no_limit_reaches_the_provider_with_the_one_reserved() {
        let (upstream, seen) = recording_upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let ingress = Arc::new(Ingress::new(vec![
            Account::new("sk-capped", "capped").with_budget(MicroUsd::from_micros(2_000_000)),
            Account::new("sk-open", "uncapped"),
        ]));
        let state = gateway_state(upstream, dir.path().join("ledger.jsonl"));
        let proxy = spawn(router(state, ingress)).await;

        for token in ["sk-capped", "sk-open"] {
            let (status, text) = send(proxy, token, &[], BODY).await;
            assert_eq!(status, 200, "{text}");
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0]["max_tokens"], DEFAULT_OUTPUT_RESERVE, "{}", seen[0]);
        // An uncapped account's request goes as it was sent.
        assert!(seen[1].get("max_tokens").is_none(), "{}", seen[1]);
    }

    /// A capped call reaches the provider asking for what admission priced
    /// and nothing more. A second limit that was not a whole number went out
    /// beside the one priced (`max_completion_tokens: 50000.0`, which a
    /// vLLM-style server reads as 50000 and prefers), and an `n` admission
    /// could not read (`"8"`) was priced as one answer and went out as it
    /// was, for a lenient server to answer eight times.
    #[tokio::test]
    async fn a_capped_call_reaches_the_provider_asking_for_no_more_than_was_priced() {
        let (upstream, seen) = recording_upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let state = gateway_state(upstream, ledger.clone());
        let message = r#""messages":[{"role":"user","content":"ping"}]"#;
        let two_limits = format!(
            r#"{{"model":"deepseek-flash","max_tokens":100,"max_completion_tokens":50000.0,{message}}}"#
        );
        let eight = format!(r#"{{"model":"deepseek-flash","max_tokens":100,"n":"8",{message}}}"#);
        let one = reserved_for(
            &state,
            &format!(r#"{{"model":"deepseek-flash","max_tokens":100,"n":1,{message}}}"#),
        );
        // Room for a few answers of 100 tokens, not eight.
        let ingress = Arc::new(Ingress::new(vec![
            Account::new("sk-roomy", "roomy").with_budget(MicroUsd::from_micros(2_000_000)),
            Account::new("sk-tight", "tight")
                .with_budget(MicroUsd::from_micros(one.micros().saturating_mul(3))),
        ]));
        let proxy = spawn(router(state, ingress)).await;

        let (status, text) = send(proxy, "sk-tight", &[], &two_limits).await;
        assert_eq!(status, 200, "priced at its 100 tokens: {text}");
        let (status, text) = send(proxy, "sk-tight", &[], &eight).await;
        assert_eq!(status, 402, "eight answers do not fit: {text}");
        let (status, text) = send(proxy, "sk-roomy", &[], &eight).await;
        assert_eq!(status, 200, "{text}");

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "the refused call never left: {seen:?}");
        assert_eq!(seen[0]["max_tokens"], 100, "{}", seen[0]);
        assert!(
            seen[0].get("max_completion_tokens").is_none(),
            "{}",
            seen[0]
        );
        assert!(seen[1]["n"].is_u64(), "{}", seen[1]);
        assert_eq!(seen[1]["n"], 8, "{}", seen[1]);
    }

    /// A capped call's lone `max_completion_tokens` was priced and went out
    /// as it was: a provider that reads only `max_tokens` (DeepSeek) ignored
    /// it and answered at its own default length, past what was reserved.
    #[tokio::test]
    async fn a_capped_calls_limit_reaches_the_provider_as_the_max_tokens_every_server_reads() {
        let (upstream, seen) = recording_upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let ingress = Arc::new(Ingress::new(vec![
            Account::new("sk-capped", "capped").with_budget(MicroUsd::from_micros(2_000_000))
        ]));
        let state = gateway_state(upstream, dir.path().join("ledger.jsonl"));
        let proxy = spawn(router(state, ingress)).await;
        let body = r#"{"model":"deepseek-flash","max_completion_tokens":40,"messages":[{"role":"user","content":"ping"}]}"#;

        let (status, text) = send(proxy, "sk-capped", &[], body).await;

        assert_eq!(status, 200, "{text}");
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["max_tokens"], 40, "{}", seen[0]);
        assert!(
            seen[0].get("max_completion_tokens").is_none(),
            "{}",
            seen[0]
        );
    }

    /// OpenAI's reasoning models refuse `max_tokens` (400
    /// `unsupported_parameter`), and a capped call is sent with one even
    /// when its caller named none: the gateway sends it again with the same
    /// limit as `max_completion_tokens`, and the account's reservation is
    /// held through the resend and released once its row is written.
    #[tokio::test]
    async fn a_capped_call_to_a_provider_that_refuses_max_tokens_is_answered_and_settled() {
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let app = {
            let seen = seen.clone();
            Router::new().route(
                "/v1/chat/completions",
                post(move |body: String| {
                    let seen = seen.clone();
                    async move {
                        let request = serde_json::from_str::<Value>(&body).unwrap();
                        let refused = request.get("max_tokens").is_some();
                        seen.lock().unwrap().push(request);
                        if refused {
                            let error = json!({"error": {
                                "message": "Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.",
                                "type": "invalid_request_error",
                                "param": "max_tokens",
                                "code": "unsupported_parameter"
                            }});
                            return (
                                StatusCode::BAD_REQUEST,
                                [(header::CONNECTION, "close")],
                                Json(error),
                            )
                                .into_response();
                        }
                        answer().into_response()
                    }
                }),
            )
        };
        let upstream = spawn(app).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let ingress = Arc::new(Ingress::new(vec![
            Account::new("sk-capped", "capped").with_budget(MicroUsd::from_micros(2_000_000))
        ]));
        let proxy = spawn(router(
            gateway_state(upstream, ledger.clone()),
            ingress.clone(),
        ))
        .await;

        let (status, text) = send(proxy, "sk-capped", &[], BODY).await;

        assert_eq!(status, 200, "{text}");
        let (rows, _) = crate::ledger::read_entries_lenient(&ledger);
        assert_eq!(rows.len(), 1, "the refusal billed nothing and has no row");
        assert_eq!(rows[0].mission, "capped");
        assert_eq!(rows[0].outcome, crate::ledger::Outcome::Completed);
        assert_eq!(ingress.reserved("capped"), MicroUsd::ZERO);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0]["max_tokens"], DEFAULT_OUTPUT_RESERVE);
        assert_eq!(seen[1]["max_completion_tokens"], DEFAULT_OUTPUT_RESERVE);
        assert!(seen[1].get("max_tokens").is_none(), "{}", seen[1]);
    }

    /// A completed row booked under `mission` that cost `micros`.
    fn row(mission: &str, micros: u64) -> crate::ledger::LedgerEntry {
        crate::ledger::LedgerEntry::settle(crate::ledger::Dispatch {
            mission: mission.into(),
            task: "t-1".into(),
            attempt: 1,
            model: "deepseek-flash".into(),
            provider: "deepseek".into(),
            usage: crate::pricing::TokenUsage::new(1_000_000, 0, 0),
            price: crate::pricing::ModelPrice {
                input_miss: MicroUsd::from_micros(micros),
                input_hit: MicroUsd::ZERO,
                output: MicroUsd::ZERO,
            },
            started_unix_ms: 0,
            ended_unix_ms: 1,
            outcome: crate::ledger::Outcome::Completed,
        })
    }

    /// Earlier releases booked an account the accounts file names nowhere
    /// under its token. Booked now under a digest of it, its spend read as
    /// zero after the upgrade, and an account that had used its cap could
    /// spend all of it again.
    #[tokio::test]
    async fn an_unnamed_accounts_spend_booked_under_its_token_still_counts_against_its_cap() {
        let (upstream, calls) = counting_upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let token = "sk-unnamed-before-the-upgrade";
        // $0.99999 of the $1.00 cap was spent under the token before the
        // upgrade, and $0.000003 under the digest since.
        crate::ledger::append_entry(&ledger, &row(token, 999_990)).unwrap();
        crate::ledger::append_entry(&ledger, &row(&unnamed_account(token), 3)).unwrap();
        crate::ledger::append_entry(&ledger, &row("sk-named", 500_000)).unwrap();
        let doc = format!(
            r#"{{"accounts":[{{"token":"{token}","budget_usd":"1.00"}},
                {{"token":"sk-named","name":"team","budget_usd":"1.00"}}]}}"#
        );
        let ingress = Ingress::parse(&doc).unwrap();
        let unnamed = ingress.authenticate(Some(token)).unwrap();
        assert_eq!(unnamed.spent(&ledger).total, MicroUsd::from_micros(999_993));
        assert_eq!(unnamed.spent(&ledger).calls, 2);
        // A named account was always booked under its name, never its token.
        let named = ingress.authenticate(Some("sk-named")).unwrap();
        assert_eq!(named.spent(&ledger).total, MicroUsd::ZERO);

        let proxy = spawn(router(
            gateway_state(upstream, ledger.clone()),
            Arc::new(ingress),
        ))
        .await;
        let (status, text) = send(proxy, token, &[], BODY).await;
        assert_eq!(status, 402, "{text}");
        assert!(!text.contains(token), "the token never shows: {text}");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "refused before dispatch");
        let (rows, _) = crate::ledger::read_entries_lenient(&ledger);
        assert_eq!(rows.len(), 3, "and nothing more is written");
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
