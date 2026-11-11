//! The harness's own HTTP/1.1 server: the gateway, the ingress and the
//! harness API (cockpit) are served by it, with no web framework.
//!
//! Blocking and thread-per-connection on `std::net`: an accept thread hands
//! each connection to a thread of its own, which reads a request, runs the
//! route's handler on the caller's Tokio runtime, writes the answer, and
//! keeps the connection for the next request unless either side closes it.
//! Handlers stay async because the gateway's metering runs there (paced
//! waits, detached upstream calls settled on blocking threads).
//!
//! The wire behaviour is the one the axum 0.8 / hyper 1 stack had, held by
//! the transcript in `src/tests/golden/harness_http_wire.json`:
//!
//! - Routing by exact path (the query string is ignored); an unknown path is
//!   `404` and a known path with another method `405` with an `allow`
//!   header, both with an empty body. A `GET` route answers `HEAD` too.
//! - A request head is limited to 417,792 bytes and 100 headers (`431`);
//!   a malformed head is answered `400` and the connection closed.
//! - Bodies arrive by `content-length` or chunked transfer coding; an
//!   `expect: 100-continue` request gets its interim `100 Continue` when the
//!   handler takes the body. A route that takes the body reads at most
//!   [`DEFAULT_BODY_LIMIT`] (2 MiB) unless it sets its own
//!   ([`MethodRouter::limit`]).
//! - A full body is framed by `content-length`; a streamed one by chunked
//!   transfer coding on HTTP/1.1, one chunk per piece the handler sends, or
//!   by closing the connection on HTTP/1.0. Every response carries `date`.
//! - A client that hangs up while its handler runs has that handler dropped
//!   at its next await, as hyper did: what the handler started detached (a
//!   provider call and its ledger row) goes on.
//! - There is no read or idle timeout, as there was none before.
//! - On shutdown ([`serve`]'s future resolves), the server stops accepting,
//!   closes idle connections at once, lets each request in flight finish
//!   and returns once the last connection is done.
//!
//! HTTP/2 (h2c prior knowledge, which axum's `http2` feature also accepted)
//! is not served: every client of these ports speaks HTTP/1.1.

use std::collections::HashMap;
use std::future::Future;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Most a request body may hold for a route that sets no limit of its own
/// (axum's `DefaultBodyLimit`).
pub const DEFAULT_BODY_LIMIT: usize = 2 * 1024 * 1024;
/// Most a request head may take (hyper's default read buffer).
const MAX_HEAD_BYTES: usize = 8192 + 4096 * 100;
/// Most headers a request may carry.
const MAX_HEADERS: usize = 100;
/// How often a running handler's client is checked for having hung up.
const HANGUP_POLL: Duration = Duration::from_millis(25);

/// An HTTP status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusCode(u16);

macro_rules! status_codes {
    ($(($code:expr, $name:ident, $reason:expr);)+) => {
        impl StatusCode {
            $(
                #[allow(missing_docs)]
                pub const $name: StatusCode = StatusCode($code);
            )+

            /// The reason phrase the standard gives the code.
            #[must_use]
            pub fn canonical_reason(self) -> Option<&'static str> {
                match self.0 {
                    $($code => Some($reason),)+
                    _ => None,
                }
            }
        }
    };
}

status_codes! {
    (100, CONTINUE, "Continue");
    (101, SWITCHING_PROTOCOLS, "Switching Protocols");
    (102, PROCESSING, "Processing");
    (103, EARLY_HINTS, "Early Hints");
    (200, OK, "OK");
    (201, CREATED, "Created");
    (202, ACCEPTED, "Accepted");
    (203, NON_AUTHORITATIVE_INFORMATION, "Non Authoritative Information");
    (204, NO_CONTENT, "No Content");
    (205, RESET_CONTENT, "Reset Content");
    (206, PARTIAL_CONTENT, "Partial Content");
    (207, MULTI_STATUS, "Multi-Status");
    (208, ALREADY_REPORTED, "Already Reported");
    (226, IM_USED, "IM Used");
    (300, MULTIPLE_CHOICES, "Multiple Choices");
    (301, MOVED_PERMANENTLY, "Moved Permanently");
    (302, FOUND, "Found");
    (303, SEE_OTHER, "See Other");
    (304, NOT_MODIFIED, "Not Modified");
    (305, USE_PROXY, "Use Proxy");
    (307, TEMPORARY_REDIRECT, "Temporary Redirect");
    (308, PERMANENT_REDIRECT, "Permanent Redirect");
    (400, BAD_REQUEST, "Bad Request");
    (401, UNAUTHORIZED, "Unauthorized");
    (402, PAYMENT_REQUIRED, "Payment Required");
    (403, FORBIDDEN, "Forbidden");
    (404, NOT_FOUND, "Not Found");
    (405, METHOD_NOT_ALLOWED, "Method Not Allowed");
    (406, NOT_ACCEPTABLE, "Not Acceptable");
    (407, PROXY_AUTHENTICATION_REQUIRED, "Proxy Authentication Required");
    (408, REQUEST_TIMEOUT, "Request Timeout");
    (409, CONFLICT, "Conflict");
    (410, GONE, "Gone");
    (411, LENGTH_REQUIRED, "Length Required");
    (412, PRECONDITION_FAILED, "Precondition Failed");
    (413, PAYLOAD_TOO_LARGE, "Payload Too Large");
    (414, URI_TOO_LONG, "URI Too Long");
    (415, UNSUPPORTED_MEDIA_TYPE, "Unsupported Media Type");
    (416, RANGE_NOT_SATISFIABLE, "Range Not Satisfiable");
    (417, EXPECTATION_FAILED, "Expectation Failed");
    (418, IM_A_TEAPOT, "I'm a teapot");
    (421, MISDIRECTED_REQUEST, "Misdirected Request");
    (422, UNPROCESSABLE_ENTITY, "Unprocessable Entity");
    (423, LOCKED, "Locked");
    (424, FAILED_DEPENDENCY, "Failed Dependency");
    (425, TOO_EARLY, "Too Early");
    (426, UPGRADE_REQUIRED, "Upgrade Required");
    (428, PRECONDITION_REQUIRED, "Precondition Required");
    (429, TOO_MANY_REQUESTS, "Too Many Requests");
    (431, REQUEST_HEADER_FIELDS_TOO_LARGE, "Request Header Fields Too Large");
    (451, UNAVAILABLE_FOR_LEGAL_REASONS, "Unavailable For Legal Reasons");
    (500, INTERNAL_SERVER_ERROR, "Internal Server Error");
    (501, NOT_IMPLEMENTED, "Not Implemented");
    (502, BAD_GATEWAY, "Bad Gateway");
    (503, SERVICE_UNAVAILABLE, "Service Unavailable");
    (504, GATEWAY_TIMEOUT, "Gateway Timeout");
    (505, HTTP_VERSION_NOT_SUPPORTED, "HTTP Version Not Supported");
    (506, VARIANT_ALSO_NEGOTIATES, "Variant Also Negotiates");
    (507, INSUFFICIENT_STORAGE, "Insufficient Storage");
    (508, LOOP_DETECTED, "Loop Detected");
    (510, NOT_EXTENDED, "Not Extended");
    (511, NETWORK_AUTHENTICATION_REQUIRED, "Network Authentication Required");
}

impl StatusCode {
    /// A status from its number; any three-digit number is one.
    ///
    /// # Errors
    ///
    /// A number outside 100–999.
    pub fn from_u16(code: u16) -> Result<Self, InvalidStatusCode> {
        if (100..=999).contains(&code) {
            Ok(Self(code))
        } else {
            Err(InvalidStatusCode)
        }
    }

    /// The number.
    #[must_use]
    pub fn as_u16(self) -> u16 {
        self.0
    }

    /// 2xx.
    #[must_use]
    pub fn is_success(self) -> bool {
        (200..300).contains(&self.0)
    }

    fn is_informational(self) -> bool {
        (100..200).contains(&self.0)
    }
}

/// A number that is not an HTTP status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidStatusCode;

/// Header names, lowercase as they go on the wire.
pub mod header {
    /// `allow`
    pub const ALLOW: &str = "allow";
    /// `authorization`
    pub const AUTHORIZATION: &str = "authorization";
    /// `cache-control`
    pub const CACHE_CONTROL: &str = "cache-control";
    /// `connection`
    pub const CONNECTION: &str = "connection";
    /// `content-length`
    pub const CONTENT_LENGTH: &str = "content-length";
    /// `content-type`
    pub const CONTENT_TYPE: &str = "content-type";
    /// `retry-after`
    pub const RETRY_AFTER: &str = "retry-after";
    /// `transfer-encoding`
    pub const TRANSFER_ENCODING: &str = "transfer-encoding";
    /// `www-authenticate`
    pub const WWW_AUTHENTICATE: &str = "www-authenticate";
}

/// A header value: any bytes but control characters other than tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderValue(Vec<u8>);

/// Bytes that cannot be a header value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidHeaderValue;

impl HeaderValue {
    /// A value written into the source; a control character in it (other
    /// than tab) is left out, so no literal can split a header.
    #[must_use]
    pub fn from_static(value: &'static str) -> Self {
        Self(value.bytes().filter(|&b| valid_value_byte(b)).collect())
    }

    /// A value from text.
    ///
    /// # Errors
    ///
    /// When `value` holds a control character other than tab.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Result<Self, InvalidHeaderValue> {
        Self::from_bytes(value.as_bytes())
    }

    /// A value from bytes (obs-text, 0x80 and above, is allowed).
    ///
    /// # Errors
    ///
    /// When `value` holds a control character other than tab.
    pub fn from_bytes(value: &[u8]) -> Result<Self, InvalidHeaderValue> {
        if value.iter().all(|&b| valid_value_byte(b)) {
            Ok(Self(value.to_vec()))
        } else {
            Err(InvalidHeaderValue)
        }
    }

    /// The value as text, when it is visible ASCII (tab and space included).
    ///
    /// # Errors
    ///
    /// When it holds any byte outside visible ASCII.
    pub fn to_str(&self) -> Result<&str, InvalidHeaderValue> {
        if self
            .0
            .iter()
            .all(|&b| b == b'\t' || (0x20..0x7f).contains(&b))
        {
            std::str::from_utf8(&self.0).map_err(|_| InvalidHeaderValue)
        } else {
            Err(InvalidHeaderValue)
        }
    }

    /// The raw bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

fn valid_value_byte(byte: u8) -> bool {
    byte == b'\t' || (byte >= 0x20 && byte != 0x7f)
}

impl From<u64> for HeaderValue {
    fn from(value: u64) -> Self {
        Self(value.to_string().into_bytes())
    }
}

/// Request or response headers, in arrival or insertion order; names are
/// compared without regard to case and kept lowercase.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderMap(Vec<(String, HeaderValue)>);

impl HeaderMap {
    /// No headers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The first value of `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&HeaderValue> {
        self.0
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }

    /// Every value of `name`, in order.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a HeaderValue> + 'a {
        self.0
            .iter()
            .filter(move |(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }

    /// Whether `name` is present.
    #[must_use]
    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Set `name` to `value` alone: it takes the place of the first value
    /// the name had, and any others go.
    pub fn insert(&mut self, name: &str, value: HeaderValue) {
        let name = name.to_ascii_lowercase();
        match self.0.iter().position(|(key, _)| *key == name) {
            Some(at) => {
                self.0[at].1 = value;
                let mut index = 0;
                self.0.retain(|(key, _)| {
                    index += 1;
                    index - 1 == at || *key != name
                });
            }
            None => self.0.push((name, value)),
        }
    }

    /// Add a value of `name`, keeping those it has.
    pub fn append(&mut self, name: &str, value: HeaderValue) {
        self.0.push((name.to_ascii_lowercase(), value));
    }

    /// Every header, in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &HeaderValue)> {
        self.0.iter().map(|(name, value)| (name.as_str(), value))
    }
}

/// A response body.
pub enum Body {
    /// All of it, sent with a `content-length`.
    Full(Vec<u8>),
    /// Pieces as the handler sends them, each one chunk on the wire; the
    /// body ends when every sender is gone. Dropped when the client goes.
    Stream(tokio::sync::mpsc::Receiver<Vec<u8>>),
}

impl Body {
    /// No bytes.
    #[must_use]
    pub fn empty() -> Self {
        Self::Full(Vec::new())
    }
}

impl From<Vec<u8>> for Body {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Full(bytes)
    }
}

impl From<String> for Body {
    fn from(text: String) -> Self {
        Self::Full(text.into_bytes())
    }
}

impl From<&'static str> for Body {
    fn from(text: &'static str) -> Self {
        Self::Full(text.as_bytes().to_vec())
    }
}

/// An answer.
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    body: Body,
}

impl Response {
    /// `200` with `body` and no headers.
    #[must_use]
    pub fn new(body: impl Into<Body>) -> Self {
        Self {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: body.into(),
        }
    }

    /// The status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The status, to change.
    pub fn status_mut(&mut self) -> &mut StatusCode {
        &mut self.status
    }

    /// The headers.
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The headers, to change.
    pub fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    /// The body.
    #[must_use]
    pub fn into_body(self) -> Body {
        self.body
    }
}

/// What a handler may answer with.
pub trait IntoResponse {
    /// The answer.
    fn into_response(self) -> Response;
}

impl IntoResponse for Response {
    fn into_response(self) -> Response {
        self
    }
}

impl IntoResponse for &'static str {
    fn into_response(self) -> Response {
        plain(self.to_string())
    }
}

impl IntoResponse for String {
    fn into_response(self) -> Response {
        plain(self)
    }
}

fn plain(text: String) -> Response {
    let mut response = Response::new(text);
    response.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// A JSON answer (`content-type: application/json`).
pub struct Json<T>(pub T);

impl<T: serde::Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.0) {
            Ok(bytes) => {
                let mut response = Response::new(bytes);
                response.headers.insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                response
            }
            Err(error) => {
                let mut response = error.to_string().into_response();
                response.status = StatusCode::INTERNAL_SERVER_ERROR;
                response
            }
        }
    }
}

impl<R: IntoResponse> IntoResponse for (StatusCode, R) {
    fn into_response(self) -> Response {
        let mut response = self.1.into_response();
        response.status = self.0;
        response
    }
}

impl<R: IntoResponse, const N: usize> IntoResponse for ([(&str, &str); N], R) {
    fn into_response(self) -> Response {
        let mut response = self.1.into_response();
        for (name, value) in self.0 {
            if let Ok(value) = HeaderValue::from_str(value) {
                response.headers.insert(name, value);
            }
        }
        response
    }
}

impl<R: IntoResponse, const N: usize> IntoResponse for (StatusCode, [(&str, &str); N], R) {
    fn into_response(self) -> Response {
        let mut response = (self.1, self.2).into_response();
        response.status = self.0;
        response
    }
}

/// One request, its body read in full when the route takes it.
pub struct Request {
    /// The method, as sent.
    pub method: String,
    /// The path, without the query.
    pub path: String,
    /// The headers.
    pub headers: HeaderMap,
    /// The body, when the route takes it; empty otherwise.
    pub body: Vec<u8>,
    /// The client's address, when the server was asked to pass it on.
    pub peer: Option<SocketAddr>,
}

impl Request {
    /// The body as text (axum's `String` extractor).
    ///
    /// # Errors
    ///
    /// `400` when the body is not UTF-8.
    pub fn text(&self) -> Result<String, Response> {
        String::from_utf8(self.body.clone()).map_err(|error| {
            rejection(
                StatusCode::BAD_REQUEST,
                &format!(
                    "Request body didn't contain valid UTF-8: {}",
                    error.utf8_error()
                ),
            )
        })
    }

    /// Whether the request says its body is JSON (`application/json`, or
    /// any `application/*+json`), as axum's `Json` extractor required.
    #[must_use]
    pub fn has_json_content_type(&self) -> bool {
        let Some(value) = self
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
        else {
            return false;
        };
        let Some((kind, rest)) = value.split_once('/') else {
            return false;
        };
        let essence = rest.split(';').next().unwrap_or("");
        let subtype = essence.trim_end();
        let valid_token = |text: &str| {
            !text.is_empty()
                && text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&b))
        };
        if !valid_token(kind) || !valid_token(subtype) || !params_parse(rest) {
            return false;
        }
        kind.eq_ignore_ascii_case("application")
            && (subtype.eq_ignore_ascii_case("json")
                || subtype
                    .rsplit_once('+')
                    .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("json")))
    }
}

impl Request {
    /// The body as JSON of type `T` (axum's `Json` extractor, with its
    /// rejections): `415` without a JSON content type, `400` for a body that
    /// is not JSON, `422` for JSON of the wrong shape. A refusal names the
    /// path of the value it is about, as axum's did.
    ///
    /// # Errors
    ///
    /// The refusal to answer with.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T, Response> {
        if !self.has_json_content_type() {
            return Err(rejection(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Expected request with `Content-Type: application/json`",
            ));
        }
        let syntax = |error: &dyn std::fmt::Display| {
            rejection(
                StatusCode::BAD_REQUEST,
                &format!("Failed to parse the request body as JSON: {error}"),
            )
        };
        let mut deserializer = serde_json::Deserializer::from_slice(&self.body);
        let error = match T::deserialize(&mut deserializer) {
            Ok(value) => {
                return deserializer
                    .end()
                    .map(|()| value)
                    .map_err(|error| syntax(&error));
            }
            Err(error) => error,
        };
        let path = json_error_path(&self.body, &error);
        let message = match path {
            Some(path) => format!("{path}: {error}"),
            None => error.to_string(),
        };
        Err(match error.classify() {
            serde_json::error::Category::Data => rejection(
                StatusCode::UNPROCESSABLE_ENTITY,
                &format!("Failed to deserialize the JSON body into the target type: {message}"),
            ),
            serde_json::error::Category::Syntax
            | serde_json::error::Category::Eof
            | serde_json::error::Category::Io => syntax(&message),
        })
    }
}

/// One step of the path to a JSON value.
#[derive(Clone)]
enum Step {
    Key(String),
    Index(usize),
    /// Between an object's entries (an entry's key or separator).
    Between,
}

/// The path axum's `Json` (through `serde_path_to_error`) put before a
/// deserialization error: for malformed JSON, the value being parsed where
/// it broke (or `?` between entries); for JSON of the wrong shape, the value
/// that ends where the error was raised. `None` where axum printed none.
fn json_error_path(body: &[u8], error: &serde_json::Error) -> Option<String> {
    let mut walk = Walk {
        bytes: body,
        at: 0,
        ends: Vec::new(),
    };
    let mut chain = Vec::new();
    let broken = walk.value(&mut chain).err();
    let path = match error.classify() {
        serde_json::error::Category::Data => {
            if error.line() == 0 {
                return None;
            }
            let offset = body
                .split(|&b| b == b'\n')
                .take(error.line() - 1)
                .map(|line| line.len() + 1)
                .sum::<usize>()
                + error.column();
            walk.ends
                .into_iter()
                .filter(|(end, _)| *end == offset)
                .max_by_key(|(_, chain)| chain.len())?
                .1
        }
        serde_json::error::Category::Syntax
        | serde_json::error::Category::Eof
        | serde_json::error::Category::Io => broken?,
    };
    if path.iter().all(|step| matches!(step, Step::Between)) {
        return None;
    }
    let mut text = String::new();
    for (index, step) in path.iter().enumerate() {
        match step {
            Step::Index(at) => text.push_str(&format!("[{at}]")),
            Step::Key(key) => {
                if index > 0 {
                    text.push('.');
                }
                text.push_str(key);
            }
            Step::Between => {
                if index > 0 {
                    text.push('.');
                }
                text.push('?');
            }
        }
    }
    Some(text)
}

/// A JSON syntax walk that records where each value ends and, at the first
/// fault, the path it was in.
struct Walk<'a> {
    bytes: &'a [u8],
    at: usize,
    ends: Vec<(usize, Vec<Step>)>,
}

impl Walk<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn value(&mut self, chain: &mut Vec<Step>) -> Result<(), Vec<Step>> {
        self.skip_space();
        let fine = match self.peek() {
            Some(b'{') => {
                self.object(chain)?;
                true
            }
            Some(b'[') => {
                self.array(chain)?;
                true
            }
            Some(b'"') => self.string(),
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => false,
        };
        if !fine {
            return Err(chain.clone());
        }
        self.ends.push((self.at, chain.clone()));
        Ok(())
    }

    /// At an entry boundary: `Ok(true)` for another entry, `Ok(false)` at
    /// the close. `on_fault` is the path a fault here is reported under.
    fn next_entry(&mut self, first: bool, close: u8, on_fault: &[Step]) -> Result<bool, Vec<Step>> {
        self.skip_space();
        match self.peek() {
            Some(byte) if byte == close => {
                self.at += 1;
                Ok(false)
            }
            Some(b',') if !first => {
                self.at += 1;
                self.skip_space();
                if self.peek() == Some(close) {
                    return Err(on_fault.to_vec());
                }
                Ok(true)
            }
            Some(_) if first => Ok(true),
            _ => Err(on_fault.to_vec()),
        }
    }

    fn object(&mut self, chain: &mut Vec<Step>) -> Result<(), Vec<Step>> {
        self.at += 1;
        let mut between = chain.clone();
        between.push(Step::Between);
        let mut first = true;
        while self.next_entry(first, b'}', &between)? {
            first = false;
            let start = self.at;
            if self.peek() != Some(b'"') || !self.string() {
                return Err(between);
            }
            let key =
                serde_json::from_slice::<String>(&self.bytes[start..self.at]).unwrap_or_default();
            self.skip_space();
            if self.peek() != Some(b':') {
                return Err(chain.clone());
            }
            self.at += 1;
            chain.push(Step::Key(key));
            let entry = self.value(chain);
            chain.pop();
            entry?;
        }
        Ok(())
    }

    fn array(&mut self, chain: &mut Vec<Step>) -> Result<(), Vec<Step>> {
        self.at += 1;
        let parent = chain.clone();
        let mut index = 0;
        while self.next_entry(index == 0, b']', &parent)? {
            chain.push(Step::Index(index));
            let element = self.value(chain);
            chain.pop();
            element?;
            index += 1;
        }
        Ok(())
    }

    fn literal(&mut self, word: &[u8]) -> bool {
        let fits = self.bytes[self.at..].starts_with(word);
        if fits {
            self.at += word.len();
        }
        fits
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> bool {
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        if self.peek() == Some(b'0') {
            self.at += 1;
        } else if self.digits() == 0 {
            return false;
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if self.digits() == 0 {
                return false;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return false;
            }
        }
        true
    }

    fn string(&mut self) -> bool {
        let start = self.at + 1;
        self.at += 1;
        loop {
            match self.peek() {
                None => return false,
                Some(b'"') => {
                    self.at += 1;
                    return std::str::from_utf8(&self.bytes[start..self.at - 1]).is_ok();
                }
                Some(b'\\') => {
                    self.at += 1;
                    match self.peek() {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            self.at += 1
                        }
                        Some(b'u') => {
                            self.at += 1;
                            for _ in 0..4 {
                                if !self.peek().is_some_and(|b| b.is_ascii_hexdigit()) {
                                    return false;
                                }
                                self.at += 1;
                            }
                        }
                        _ => return false,
                    }
                }
                Some(byte) if byte < 0x20 => return false,
                Some(_) => self.at += 1,
            }
        }
    }
}

/// Whether the parameters after a media type's subtype parse
/// (`; name=value` pairs, values tokens or quoted strings).
fn params_parse(after_slash: &str) -> bool {
    let mut parts = after_slash.split(';');
    let _ = parts.next();
    for part in parts {
        let part = part.trim_start();
        if part.is_empty() {
            continue;
        }
        let Some((name, value)) = part.split_once('=') else {
            return false;
        };
        if name.is_empty() || name.contains(' ') || value.trim_end().is_empty() {
            return false;
        }
    }
    true
}

/// A plain-text refusal in axum's rejection shape.
#[must_use]
pub fn rejection(status: StatusCode, message: &str) -> Response {
    (status, message.to_string()).into_response()
}

/// A boxed handler future.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
type Handler = Arc<dyn Fn(Request) -> BoxFuture<Response> + Send + Sync>;

fn boxed<H, F, R>(handler: H) -> Handler
where
    H: Fn(Request) -> F + Send + Sync + 'static,
    F: Future<Output = R> + Send + 'static,
    R: IntoResponse,
{
    Arc::new(move |request| {
        let future = handler(request);
        Box::pin(async move { future.await.into_response() })
    })
}

/// A check run on every request to a path before its handler (or its
/// `405`): `Some` is the answer instead.
pub type Guard = Arc<dyn Fn(&Request) -> Option<Response> + Send + Sync>;

/// The handlers of one path, by method.
pub struct MethodRouter {
    get: Option<Handler>,
    post: Option<Handler>,
    /// A guard and the most body it reads.
    guard: Option<(Guard, usize)>,
}

/// A path answered on `GET` (and `HEAD`); its handler gets no body.
pub fn get<H, F, R>(handler: H) -> MethodRouter
where
    H: Fn(Request) -> F + Send + Sync + 'static,
    F: Future<Output = R> + Send + 'static,
    R: IntoResponse,
{
    MethodRouter {
        get: Some(boxed(handler)),
        post: None,
        guard: None,
    }
}

/// A path answered on `POST`; its handler gets the body, read in full, of
/// at most [`DEFAULT_BODY_LIMIT`] bytes (a longer one is answered `413`
/// with axum's rejection text).
pub fn post<H, F, R>(handler: H) -> MethodRouter
where
    H: Fn(Request) -> F + Send + Sync + 'static,
    F: Future<Output = R> + Send + 'static,
    R: IntoResponse,
{
    MethodRouter {
        get: None,
        post: Some(boxed(handler)),
        guard: None,
    }
}

impl MethodRouter {
    /// Put `guard` in front of every request to the path, whatever its
    /// method (an axum route layer): the body is read first, at most `limit`
    /// bytes (a longer one is answered `413` with an empty body), and the
    /// guard sees it before any handler does.
    #[must_use]
    pub fn guard(mut self, limit: usize, guard: Guard) -> Self {
        self.guard = Some((guard, limit));
        self
    }

    fn allow(&self) -> &'static str {
        match (self.get.is_some(), self.post.is_some()) {
            (true, true) => "GET,HEAD,POST",
            (true, false) => "GET,HEAD",
            (false, true) => "POST",
            (false, false) => "",
        }
    }
}

/// Paths and their handlers.
#[derive(Default)]
pub struct Router {
    routes: Vec<(String, MethodRouter)>,
    /// Every answer closes its connection.
    closing: bool,
}

impl Router {
    /// No routes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer `path` with `methods`.
    #[must_use]
    pub fn route(mut self, path: &str, methods: MethodRouter) -> Self {
        self.routes.push((path.to_string(), methods));
        self
    }

    /// Close the connection after every answer (`connection: close`), so
    /// no client keeps one to this server pooled.
    #[must_use]
    pub fn closing(mut self) -> Self {
        self.closing = true;
        self
    }

    fn find(&self, path: &str) -> Option<&MethodRouter> {
        self.routes
            .iter()
            .find(|(route, _)| route == path)
            .map(|(_, methods)| methods)
    }
}

/// Whether handlers see the client's address.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Peer {
    /// [`Request::peer`] is the connection's source address.
    Pass,
    /// [`Request::peer`] is `None`.
    Withhold,
}

/// Serve `router` on `listener` until `shutdown` resolves, then stop
/// accepting, close idle connections, finish the requests in flight and
/// return.
///
/// Must be called on a Tokio runtime: handlers run on it.
///
/// # Errors
///
/// The listener cannot be taken over (moved to blocking mode or read).
pub async fn serve<S>(
    listener: tokio::net::TcpListener,
    router: Router,
    peer: Peer,
    shutdown: S,
) -> io::Result<()>
where
    S: Future<Output = ()> + Send,
{
    let listener = listener.into_std()?;
    listener.set_nonblocking(false)?;
    let local = listener.local_addr()?;
    let shared = Arc::new(Shared {
        router,
        peer,
        runtime: tokio::runtime::Handle::current(),
        stopping: AtomicBool::new(false),
        next_id: AtomicU64::new(0),
        connections: Mutex::new(HashMap::new()),
        drained: Condvar::new(),
    });
    let acceptor = {
        let shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("harness-http-accept".into())
            .spawn(move || accept_loop(&listener, &shared))?
    };
    // Dropped (its task aborted, its runtime gone), the server stops too.
    let stop = Stop {
        shared: Arc::clone(&shared),
        local,
    };
    shutdown.await;
    drop(stop);
    tokio::task::spawn_blocking(move || {
        let _ = acceptor.join();
        shared.wait_drained();
    })
    .await
    .map_err(io::Error::other)
}

/// Stops a server when dropped: no new connections, idle ones closed.
struct Stop {
    shared: Arc<Shared>,
    local: SocketAddr,
}

impl Drop for Stop {
    fn drop(&mut self) {
        self.shared.stopping.store(true, Ordering::SeqCst);
        // Wake the accept thread with a connection of its own.
        let _ = TcpStream::connect_timeout(&wake_address(self.local), Duration::from_secs(1));
        self.shared.close_idle();
    }
}

fn wake_address(local: SocketAddr) -> SocketAddr {
    let loopback = match local.ip() {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
    };
    if local.ip().is_unspecified() {
        SocketAddr::new(loopback, local.port())
    } else {
        local
    }
}

struct Connection {
    stream: TcpStream,
    idle: bool,
}

struct Shared {
    router: Router,
    peer: Peer,
    runtime: tokio::runtime::Handle,
    stopping: AtomicBool,
    next_id: AtomicU64,
    connections: Mutex<HashMap<u64, Connection>>,
    drained: Condvar,
}

impl Shared {
    fn connections(&self) -> std::sync::MutexGuard<'_, HashMap<u64, Connection>> {
        self.connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Mark a connection idle (between requests) or busy. `false` when the
    /// server is stopping and an idle connection should close.
    fn set_idle(&self, id: u64, idle: bool) -> bool {
        let mut connections = self.connections();
        if let Some(connection) = connections.get_mut(&id) {
            connection.idle = idle;
        }
        !(idle && self.stopping.load(Ordering::SeqCst))
    }

    fn close_idle(&self) {
        for connection in self.connections().values() {
            if connection.idle {
                let _ = connection.stream.shutdown(Shutdown::Both);
            }
        }
    }

    fn remove(&self, id: u64) {
        let mut connections = self.connections();
        connections.remove(&id);
        if connections.is_empty() {
            self.drained.notify_all();
        }
    }

    fn wait_drained(&self) {
        let mut connections = self.connections();
        while !connections.is_empty() {
            connections = self
                .drained
                .wait(connections)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>) {
    loop {
        match listener.accept() {
            Ok((stream, peer)) => {
                if shared.stopping.load(Ordering::SeqCst) {
                    return;
                }
                let _ = stream.set_nonblocking(false);
                let Ok(handle) = stream.try_clone() else {
                    continue;
                };
                let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
                shared.connections().insert(
                    id,
                    Connection {
                        stream: handle,
                        idle: true,
                    },
                );
                let worker = Arc::clone(shared);
                let spawned = std::thread::Builder::new()
                    .name("harness-http-conn".into())
                    .spawn(move || {
                        serve_connection(&worker, id, stream, peer);
                        worker.remove(id);
                    });
                if spawned.is_err() {
                    shared.remove(id);
                }
            }
            Err(error) => {
                if shared.stopping.load(Ordering::SeqCst) {
                    return;
                }
                if !matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                ) {
                    // Out of descriptors, most likely: wait for some to close.
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }
}

/// HTTP version of a request.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Version {
    Http10,
    Http11,
}

impl Version {
    fn as_str(self) -> &'static str {
        match self {
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
        }
    }
}

/// How a request's body is framed.
#[derive(Clone, Copy)]
enum Framing {
    Length(u64),
    Chunked,
}

struct Head {
    method: String,
    target: String,
    version: Version,
    headers: HeaderMap,
    framing: Framing,
    keep_alive: bool,
    expect_continue: bool,
}

/// Why a request head could not be read.
enum HeadError {
    /// The connection closed or failed before a complete head.
    Gone,
    /// Malformed (`400`) or too large (`431`): answered, then closed.
    Refused(StatusCode),
}

fn serve_connection(shared: &Shared, id: u64, stream: TcpStream, peer: SocketAddr) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::with_capacity(8192, read_half);
    let mut writer = stream;
    serve_requests(shared, id, &mut reader, &mut writer, peer);
    // Close as hyper did: the client reads the end of the stream before
    // the socket goes, even with request bytes still unread.
    let _ = writer.shutdown(Shutdown::Write);
}

fn serve_requests(
    shared: &Shared,
    id: u64,
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    peer: SocketAddr,
) {
    loop {
        if !shared.set_idle(id, true) {
            return;
        }
        let head = match read_head(reader) {
            Ok(head) => head,
            Err(HeadError::Gone) => return,
            Err(HeadError::Refused(status)) => {
                let _ = write_refusal(writer, status);
                return;
            }
        };
        shared.set_idle(id, false);
        match serve_request(shared, reader, writer, &head, peer) {
            Ok(true) => {}
            Ok(false) | Err(_) => return,
        }
    }
}

/// Answer one request; `Ok(true)` when the connection is at the next one.
fn serve_request(
    shared: &Shared,
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    head: &Head,
    peer: SocketAddr,
) -> io::Result<bool> {
    let path = request_path(&head.target);
    let mut body_read = matches!(head.framing, Framing::Length(0));
    let response = match shared.router.find(&path) {
        None => bare(StatusCode::NOT_FOUND),
        Some(methods) => {
            let handler = match head.method.as_str() {
                "GET" | "HEAD" => methods.get.clone(),
                "POST" => methods.post.clone(),
                _ => None,
            };
            let guard_limit = methods.guard.as_ref().map(|(_, limit)| *limit);
            let takes_body = guard_limit.is_some() || (handler.is_some() && head.method == "POST");
            let mut early = None;
            let mut body = Vec::new();
            if takes_body {
                match read_body(
                    reader,
                    writer,
                    head,
                    guard_limit.unwrap_or(DEFAULT_BODY_LIMIT),
                )? {
                    BodyRead::Complete(bytes) => {
                        body = bytes;
                        body_read = true;
                    }
                    BodyRead::TooLarge { drained } => {
                        body_read = drained;
                        early = Some(match guard_limit {
                            Some(_) => bare(StatusCode::PAYLOAD_TOO_LARGE),
                            None => rejection(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "Failed to buffer the request body: length limit exceeded",
                            ),
                        });
                    }
                    BodyRead::Broken => return Ok(false),
                }
            }
            let request = Request {
                method: head.method.clone(),
                path,
                headers: head.headers.clone(),
                body,
                peer: (shared.peer == Peer::Pass).then_some(peer),
            };
            if early.is_none() {
                if let Some((guard, _)) = &methods.guard {
                    early = guard(&request);
                }
            }
            let allowed = handler.is_some();
            let mut response = match (early, handler) {
                (Some(response), _) => response,
                (None, Some(handler)) => match run_handler(shared, writer, &handler, request) {
                    Some(response) => response,
                    None => return Ok(false),
                },
                (None, None) => bare(StatusCode::METHOD_NOT_ALLOWED),
            };
            if !allowed && !response.headers.contains_key(header::ALLOW) {
                response
                    .headers
                    .insert(header::ALLOW, HeaderValue::from_static(methods.allow()));
            }
            response
        }
    };
    // A client that asked to send its body only once told to continue was
    // never told: it will not send it, so nothing follows on the connection.
    let keep_alive = head.keep_alive
        && !shared.router.closing
        && !shared.stopping.load(Ordering::SeqCst)
        && (body_read || !head.expect_continue);
    if !write_response(writer, head, response, keep_alive)? {
        return Ok(false);
    }
    // A body nobody took is read past, so the next request can be read.
    Ok(body_read || drain_body(reader, head))
}

fn request_path(target: &str) -> String {
    let without_scheme = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
        .map(|rest| rest.find('/').map_or("/", |at| &rest[at..]));
    let target = without_scheme.unwrap_or(target);
    target.split('?').next().unwrap_or("").to_string()
}

fn bare(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    response.status = status;
    response
}

/// Run `handler` on the runtime, dropping it if the client hangs up first.
fn run_handler(
    shared: &Shared,
    stream: &TcpStream,
    handler: &Handler,
    request: Request,
) -> Option<Response> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let future = handler(request);
    let task = shared.runtime.spawn(async move {
        let _ = sender.send(future.await);
    });
    loop {
        match receiver.recv_timeout(HANGUP_POLL) {
            Ok(response) => return Some(response),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if client_gone(stream) {
                    task.abort();
                    return None;
                }
            }
        }
    }
}

/// Whether the client closed its side of the connection (or it failed).
fn client_gone(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut probe = [0u8; 1];
    let gone = match stream.peek(&mut probe) {
        Ok(0) => true,
        Ok(_) => false,
        Err(error) => error.kind() != io::ErrorKind::WouldBlock,
    };
    let _ = stream.set_nonblocking(false);
    gone
}

fn read_head(reader: &mut BufReader<TcpStream>) -> Result<Head, HeadError> {
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut total = 0usize;
    loop {
        let mut line = Vec::new();
        let room = (MAX_HEAD_BYTES - total) as u64 + 1;
        let read = (&mut *reader)
            .take(room)
            .read_until(b'\n', &mut line)
            .map_err(|_| HeadError::Gone)?;
        if read == 0 {
            return Err(if lines.is_empty() {
                HeadError::Gone
            } else {
                HeadError::Refused(StatusCode::BAD_REQUEST)
            });
        }
        total += read;
        if total > MAX_HEAD_BYTES {
            return Err(HeadError::Refused(
                StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            ));
        }
        if !line.ends_with(b"\n") {
            return Err(HeadError::Gone);
        }
        line.pop();
        if line.ends_with(b"\r") {
            line.pop();
        }
        if line.is_empty() {
            if lines.is_empty() {
                // Empty lines before a request line are skipped.
                continue;
            }
            break;
        }
        lines.push(line);
        if lines.len() > MAX_HEADERS + 1 {
            return Err(HeadError::Refused(
                StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,
            ));
        }
    }
    parse_head(&lines).ok_or(HeadError::Refused(StatusCode::BAD_REQUEST))
}

fn is_token(text: &[u8]) -> bool {
    !text.is_empty()
        && text
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn parse_head(lines: &[Vec<u8>]) -> Option<Head> {
    let (request_line, header_lines) = lines.split_first()?;
    let mut parts = request_line.split(|&b| b == b' ');
    let method = parts.next()?;
    let target = parts.next()?;
    let version = parts.next()?;
    if parts.next().is_some()
        || !is_token(method)
        || target.is_empty()
        || !target.iter().all(|&b| b > 0x20 && b != 0x7f)
    {
        return None;
    }
    let version = match version {
        b"HTTP/1.1" => Version::Http11,
        b"HTTP/1.0" => Version::Http10,
        _ => return None,
    };
    let mut headers = HeaderMap::new();
    for line in header_lines {
        let colon = line.iter().position(|&b| b == b':')?;
        let (name, value) = (&line[..colon], &line[colon + 1..]);
        if !is_token(name) {
            return None;
        }
        let value = HeaderValue::from_bytes(trim_ows(value)).ok()?;
        headers.append(std::str::from_utf8(name).ok()?, value);
    }
    let framing = framing(&headers, version)?;
    let connection = |token: &str| {
        headers.get_all(header::CONNECTION).any(|value| {
            value.to_str().is_ok_and(|value| {
                value
                    .split(',')
                    .any(|part| part.trim().eq_ignore_ascii_case(token))
            })
        })
    };
    let keep_alive = match version {
        Version::Http11 => !connection("close"),
        Version::Http10 => connection("keep-alive") && !connection("close"),
    };
    let expect_continue = version == Version::Http11
        && headers
            .get("expect")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"));
    Some(Head {
        method: String::from_utf8(method.to_vec()).ok()?,
        target: String::from_utf8(target.to_vec()).ok()?,
        version,
        headers,
        framing,
        keep_alive,
        expect_continue,
    })
}

fn trim_ows(mut value: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = value {
        value = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = value {
        value = rest;
    }
    value
}

/// The body framing a request declares; `None` for a malformed one.
fn framing(headers: &HeaderMap, version: Version) -> Option<Framing> {
    let codings: Vec<&HeaderValue> = headers.get_all(header::TRANSFER_ENCODING).collect();
    if !codings.is_empty() {
        if version == Version::Http10 {
            return None;
        }
        let last = codings.last()?.to_str().ok()?;
        let chunked_last = last
            .rsplit(',')
            .next()
            .is_some_and(|coding| coding.trim().eq_ignore_ascii_case("chunked"));
        return chunked_last.then_some(Framing::Chunked);
    }
    let mut length: Option<u64> = None;
    for value in headers.get_all(header::CONTENT_LENGTH) {
        for part in value.to_str().ok()?.split(',') {
            let part = part.trim();
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let parsed: u64 = part.parse().ok()?;
            if length.is_some_and(|known| known != parsed) {
                return None;
            }
            length = Some(parsed);
        }
    }
    Some(Framing::Length(length.unwrap_or(0)))
}

enum BodyRead {
    Complete(Vec<u8>),
    /// Longer than the limit: read to its end (`drained`) or not at all.
    TooLarge {
        drained: bool,
    },
    /// The connection failed or the framing was malformed.
    Broken,
}

fn read_body(
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    head: &Head,
    limit: usize,
) -> io::Result<BodyRead> {
    match head.framing {
        Framing::Length(0) => return Ok(BodyRead::Complete(Vec::new())),
        Framing::Length(length) if length > limit as u64 => {
            return Ok(BodyRead::TooLarge { drained: false })
        }
        Framing::Length(_) | Framing::Chunked => {}
    }
    if head.expect_continue {
        writer.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        writer.flush()?;
    }
    Ok(match head.framing {
        Framing::Length(length) => {
            let mut body = vec![0u8; usize::try_from(length).unwrap_or(usize::MAX)];
            match reader.read_exact(&mut body) {
                Ok(()) => BodyRead::Complete(body),
                Err(_) => BodyRead::Broken,
            }
        }
        Framing::Chunked => read_chunked(reader, limit),
    })
}

/// Read a chunked body to its end, keeping at most `limit` bytes of it.
fn read_chunked(reader: &mut BufReader<TcpStream>, limit: usize) -> BodyRead {
    let mut body = Vec::new();
    let mut over = false;
    let mut line = Vec::new();
    let read_line = |reader: &mut BufReader<TcpStream>, line: &mut Vec<u8>| {
        line.clear();
        matches!((&mut *reader).take(4096).read_until(b'\n', line), Ok(n) if n > 0 && line.ends_with(b"\n"))
    };
    loop {
        if !read_line(reader, &mut line) {
            return BodyRead::Broken;
        }
        let text = String::from_utf8_lossy(&line);
        let size_text = text.trim_end().split(';').next().unwrap_or("").trim();
        let Ok(size) = u64::from_str_radix(size_text, 16) else {
            return BodyRead::Broken;
        };
        if size == 0 {
            // Trailers, up to the empty line.
            loop {
                if !read_line(reader, &mut line) {
                    return BodyRead::Broken;
                }
                if line == b"\r\n" || line == b"\n" {
                    return if over {
                        BodyRead::TooLarge { drained: true }
                    } else {
                        BodyRead::Complete(body)
                    };
                }
            }
        }
        let fits = !over && body.len() as u64 + size <= limit as u64;
        let copied = if fits {
            let start = body.len();
            body.resize(start + usize::try_from(size).unwrap_or(0), 0);
            reader.read_exact(&mut body[start..]).map(|()| size)
        } else {
            over = true;
            body = Vec::new();
            io::copy(&mut (&mut *reader).take(size), &mut io::sink())
        };
        if !copied.is_ok_and(|copied| copied == size) || !read_line(reader, &mut line) {
            return BodyRead::Broken;
        }
        if line != b"\r\n" && line != b"\n" {
            return BodyRead::Broken;
        }
    }
}

/// Read past a body nobody took; `true` when the connection is then at the
/// next request.
fn drain_body(reader: &mut BufReader<TcpStream>, head: &Head) -> bool {
    match head.framing {
        Framing::Length(length) => io::copy(&mut (&mut *reader).take(length), &mut io::sink())
            .is_ok_and(|copied| copied == length),
        Framing::Chunked => !matches!(read_chunked(reader, 0), BodyRead::Broken),
    }
}

/// The answer to a head that cannot be read.
fn write_refusal(writer: &mut TcpStream, status: StatusCode) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\nconnection: close\r\ncontent-length: 0\r\ndate: {}\r\n\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("<none>"),
        http_date(SystemTime::now())
    );
    writer.write_all(head.as_bytes())?;
    writer.flush()
}

/// Write `response`; `Ok(true)` when the connection may serve another
/// request.
fn write_response(
    writer: &mut TcpStream,
    head: &Head,
    response: Response,
    keep_alive: bool,
) -> io::Result<bool> {
    let Response {
        mut status,
        mut headers,
        mut body,
    } = response;
    if status.is_informational() {
        // hyper refused to send a 1xx as a final answer.
        status = StatusCode::INTERNAL_SERVER_ERROR;
        headers = HeaderMap::new();
        body = Body::empty();
    }
    let no_body = head.method == "HEAD"
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED;
    let close_delimited = head.version == Version::Http10 && matches!(body, Body::Stream(_));
    let asked_close = headers.get_all(header::CONNECTION).any(|value| {
        value.to_str().is_ok_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("close"))
        })
    });
    let keep_alive = keep_alive && !asked_close && !(close_delimited && !no_body);
    // The length goes in after the handler's own headers, and a connection
    // header after it (axum set the one, hyper the other).
    if let Body::Full(bytes) = &body {
        if status != StatusCode::NO_CONTENT && status != StatusCode::NOT_MODIFIED {
            headers.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from(bytes.len() as u64),
            );
        }
    }
    match head.version {
        Version::Http11 if !keep_alive => {
            headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
        }
        Version::Http10 if keep_alive => {
            headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        }
        Version::Http11 | Version::Http10 => {}
    }
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(
        format!(
            "{} {} {}\r\n",
            head.version.as_str(),
            status.as_u16(),
            status.canonical_reason().unwrap_or("<none>")
        )
        .as_bytes(),
    );
    for (name, value) in headers.iter() {
        let streamed_length = name == header::CONTENT_LENGTH && matches!(body, Body::Stream(_));
        if streamed_length || name == header::TRANSFER_ENCODING {
            continue;
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    let chunked = match &body {
        Body::Full(_) => false,
        Body::Stream(_) => {
            let chunked = !no_body && head.version == Version::Http11;
            if chunked {
                out.extend_from_slice(b"transfer-encoding: chunked\r\n");
            }
            chunked
        }
    };
    out.extend_from_slice(format!("date: {}\r\n\r\n", http_date(SystemTime::now())).as_bytes());
    match body {
        Body::Full(bytes) => {
            if !no_body {
                out.extend_from_slice(&bytes);
            }
            writer.write_all(&out)?;
            writer.flush()?;
        }
        Body::Stream(mut pieces) => {
            writer.write_all(&out)?;
            writer.flush()?;
            if no_body {
                return Ok(keep_alive);
            }
            while let Some(piece) = pieces.blocking_recv() {
                if piece.is_empty() {
                    continue;
                }
                let mut frame = Vec::with_capacity(piece.len() + 12);
                if chunked {
                    frame.extend_from_slice(format!("{:X}\r\n", piece.len()).as_bytes());
                }
                frame.extend_from_slice(&piece);
                if chunked {
                    frame.extend_from_slice(b"\r\n");
                }
                // A client that went drops the body: its sender sees that.
                writer.write_all(&frame)?;
                writer.flush()?;
            }
            if chunked {
                writer.write_all(b"0\r\n\r\n")?;
                writer.flush()?;
            }
        }
    }
    Ok(keep_alive)
}

/// `time` as an HTTP date (RFC 9110 IMF-fixdate).
fn http_date(time: SystemTime) -> String {
    let seconds = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let of_day = seconds % 86_400;
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]
        [usize::try_from(days.rem_euclid(7)).unwrap_or(0)];
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let month_name = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][usize::try_from(month - 1).unwrap_or(0)];
    format!(
        "{weekday}, {day:02} {month_name} {year} {:02}:{:02}:{:02} GMT",
        of_day / 3_600,
        of_day / 60 % 60,
        of_day % 60
    )
}

#[cfg(test)]
#[path = "tests/http.rs"]
mod tests;
