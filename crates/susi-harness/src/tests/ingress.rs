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
    let expected =
        estimate_tokens(price, body, 64_000).saturating_add(estimate_tokens(price, body, 66_048));
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

/// Serve `app` for one test. The server reads every request in full before
/// it answers: a connection closed (these servers answer `connection:
/// close`) with part of a request unread is reset on Windows, and the
/// gateway's call to it failed with os error 10054.
async fn spawn(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(crate::http::serve(
        listener,
        app,
        crate::http::Peer::Withhold,
        std::future::pending(),
    ));
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
        post(move |_| {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                answer()
            }
        }),
    );
    (spawn(app).await, calls)
}

const BODY: &str = r#"{"model":"deepseek-flash","messages":[{"role":"user","content":"ping"}]}"#;

#[tokio::test]
async fn a_caller_cannot_move_its_spend_off_its_account_with_its_own_mission() {
    let (upstream, _) = counting_upstream().await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
            post(move |_| {
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
            post(move |request: Request| {
                let body = String::from_utf8(request.body).unwrap();
                let (arrived, gate) = (arrived.clone(), gate.clone());
                async move {
                    arrived.notify_one();
                    if !body.contains(r#""stream":true"#) {
                        let _open = gate.acquire().await;
                        return answer().into_response();
                    }
                    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
                    tokio::spawn(async move {
                        let first = "data: {\"id\":\"cmpl-s\",\"choices\":[{\"delta\":{\"content\":\"PARTIAL\"}}]}\n\n";
                        let _ = tx.send(first.as_bytes().to_vec()).await;
                        let _open = gate.acquire().await;
                        let rest = concat!(
                            "data: {\"id\":\"cmpl-s\",\"choices\":[],\"usage\":",
                            "{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\n",
                            "data: [DONE]\n\n"
                        );
                        let _ = tx.send(rest.as_bytes().to_vec()).await;
                    });
                    (
                        [
                            (header::CONTENT_TYPE, "text/event-stream"),
                            (header::CONNECTION, "close"),
                        ],
                        Response::new(crate::http::Body::Stream(rx)),
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
        let dir = susi_paths::tempfile::tempdir().unwrap();
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
            post(move |request: Request| {
                let body = String::from_utf8(request.body).unwrap();
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
            post(move |request: Request| {
                let body = String::from_utf8(request.body).unwrap();
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
