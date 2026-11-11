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
use crate::http::{get, header, post, HeaderMap, HeaderValue, IntoResponse, Json, Request};
use crate::http::{Peer, Response, Router, StatusCode};
use crate::money::MicroUsd;
use crate::providers::Registry;
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
    let state = IngressState { gateway, ingress };
    let models_state = state.clone();
    Router::new()
        .route("/health", get(|_| health()))
        .route(
            "/v1/models",
            get(move |request| models(models_state.clone(), request)),
        )
        .route(
            "/v1/chat/completions",
            post(move |request| completions(state.clone(), request)),
        )
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
    crate::http::serve(
        listener,
        router(gateway, ingress),
        Peer::Withhold,
        crate::gateway::shutdown_signal(),
    )
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

async fn models(state: IngressState, request: Request) -> Response {
    match state.ingress.authenticate(token_from(&request.headers)) {
        Ok(_) => Json(models_payload()).into_response(),
        Err(denial) => deny(denial),
    }
}

async fn completions(state: IngressState, request: Request) -> Response {
    let body = match request.text() {
        Ok(body) => body,
        Err(rejection) => return rejection,
    };
    let headers = request.headers;
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
#[path = "tests/ingress.rs"]
mod tests;
