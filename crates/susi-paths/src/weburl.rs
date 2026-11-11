//! `http://` and `https://` URLs without the `url` crate (T-SMALLFACADES-1).
//!
//! [`WebUrl::parse`] accepts the form a WHATWG URL parser leaves unchanged
//! apart from the normalizations it reproduces exactly: the scheme and an
//! ASCII host lowercased, a default port dropped, leading zeros of a port
//! dropped, an empty path written `/`, and an IPv6 literal in its canonical
//! text. Everything the WHATWG parser would rewrite in another way is
//! refused with an error instead of being guessed at: whitespace and control
//! chars, non-ASCII text, `\`, userinfo, `xn--` labels and percent signs in
//! the host, IPv4 hosts not written as four plain decimal numbers, dot
//! segments, and chars the parser would percent-encode. On every URL it
//! accepts, scheme, host, port, path, query, fragment and the serialization
//! equal what the `url` crate reports (checked against url 2.5.8).
//!
//! [`is_absolute_uri`] answers "would the `url` crate parse this?" for any
//! scheme, again refusing (answering `false`) where an exact answer needs
//! Unicode IDNA tables: a special-scheme host with non-ASCII text, a `%`, or
//! an `xn--` label.
use std::net::Ipv6Addr;

/// A parsed `http`/`https` URL; see the module docs for the accepted form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebUrl {
    serialized: String,
    scheme: &'static str,
    /// Byte ranges into `serialized`.
    host: (usize, usize),
    path: (usize, usize),
    query: Option<(usize, usize)>,
    fragment: Option<usize>,
    port: Option<u16>,
}

fn refuse(input: &str, why: &str) -> String {
    format!("unsupported URL {input:?}: {why}")
}

/// Chars a WHATWG parser percent-encodes in a path, query or fragment of a
/// special URL (beyond controls, space and non-ASCII, refused earlier).
const PATH_ENCODED: &[u8] = b"\"<>`{}^|";
const QUERY_ENCODED: &[u8] = b"\"<>'";
const FRAGMENT_ENCODED: &[u8] = b"\"<>`";

impl WebUrl {
    pub fn parse(input: &str) -> Result<Self, String> {
        if let Some(c) = input.chars().find(|c| !c.is_ascii_graphic()) {
            return Err(refuse(input, &format!("char {c:?}")));
        }
        if input.contains('\\') {
            return Err(refuse(input, "backslash"));
        }
        let (scheme, rest) = input
            .split_once(':')
            .ok_or_else(|| refuse(input, "no scheme"))?;
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "http" => "http",
            "https" => "https",
            other => return Err(refuse(input, &format!("scheme {other:?} is not http(s)"))),
        };
        let rest = rest
            .strip_prefix("//")
            .ok_or_else(|| refuse(input, "no `//` after the scheme"))?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, rest) = rest.split_at(end);
        if authority.contains('@') {
            return Err(refuse(input, "userinfo"));
        }
        let (host, port) = split_host_port(authority).ok_or_else(|| refuse(input, "port"))?;
        let host = canonical_host(host).ok_or_else(|| refuse(input, "host"))?;
        let default = if scheme == "http" { 80 } else { 443 };
        let port = match port {
            None | Some("") => None,
            Some(digits) => match parse_port(digits) {
                Some(port) if port == default => None,
                Some(port) => Some(port),
                None => return Err(refuse(input, "port")),
            },
        };
        let (rest, fragment) = match rest.split_once('#') {
            Some((rest, fragment)) => (rest, Some(fragment)),
            None => (rest, None),
        };
        let (path, query) = match rest.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (rest, None),
        };
        let path = if path.is_empty() { "/" } else { path };
        let encoded = |part: &str, set: &[u8]| part.bytes().any(|b| set.contains(&b));
        if encoded(path, PATH_ENCODED)
            || query.is_some_and(|q| encoded(q, QUERY_ENCODED))
            || fragment.is_some_and(|f| encoded(f, FRAGMENT_ENCODED))
        {
            return Err(refuse(input, "a char the URL parser would percent-encode"));
        }
        if path.split('/').any(is_dot_segment) {
            return Err(refuse(input, "dot segment"));
        }
        let mut serialized = format!("{scheme}://");
        let host_start = serialized.len();
        serialized.push_str(&host);
        let host = (host_start, serialized.len());
        if let Some(port) = port {
            serialized.push_str(&format!(":{port}"));
        }
        let path_start = serialized.len();
        serialized.push_str(path);
        let path = (path_start, serialized.len());
        let query = query.map(|query| {
            serialized.push('?');
            let start = serialized.len();
            serialized.push_str(query);
            (start, serialized.len())
        });
        let fragment = fragment.map(|fragment| {
            serialized.push('#');
            let start = serialized.len();
            serialized.push_str(fragment);
            start
        });
        Ok(Self {
            serialized,
            scheme,
            host,
            path,
            query,
            fragment,
            port,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.serialized
    }

    /// `http` or `https`.
    pub fn scheme(&self) -> &'static str {
        self.scheme
    }

    /// The host as serialized (an IPv6 literal keeps its brackets).
    pub fn host_str(&self) -> &str {
        &self.serialized[self.host.0..self.host.1]
    }

    /// The port when it is not the scheme's default.
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    pub fn port_or_known_default(&self) -> u16 {
        self.port
            .unwrap_or(if self.scheme == "http" { 80 } else { 443 })
    }

    /// Scheme, host and port: two URLs share an origin when these are equal.
    pub fn origin(&self) -> (&'static str, &str, u16) {
        (self.scheme, self.host_str(), self.port_or_known_default())
    }

    /// The path, always starting with `/`.
    pub fn path(&self) -> &str {
        &self.serialized[self.path.0..self.path.1]
    }

    /// The `/`-separated segments of the path, as they are written.
    pub fn path_segments(&self) -> std::str::Split<'_, char> {
        self.path()[1..].split('/')
    }

    pub fn query(&self) -> Option<&str> {
        self.query.map(|(start, end)| &self.serialized[start..end])
    }

    pub fn fragment(&self) -> Option<&str> {
        self.fragment.map(|start| &self.serialized[start..])
    }
}

impl std::fmt::Display for WebUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.serialized)
    }
}

fn is_dot_segment(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "." | ".." | "%2e" | ".%2e" | "%2e." | "%2e%2e"
    )
}

/// `host` and the text after its `:`, if any (`[v6]:port` aware).
fn split_host_port(authority: &str) -> Option<(&str, Option<&str>)> {
    if authority.starts_with('[') {
        let close = authority.find(']')?;
        let (host, rest) = authority.split_at(close + 1);
        return match rest {
            "" => Some((host, None)),
            _ => Some((host, Some(rest.strip_prefix(':')?))),
        };
    }
    match authority.split_once(':') {
        Some((host, port)) => Some((host, Some(port))),
        None => Some((authority, None)),
    }
}

/// A port as WHATWG reads it: ASCII digits, leading zeros allowed, at most
/// 65535.
fn parse_port(digits: &str) -> Option<u16> {
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let value = digits
        .trim_start_matches('0')
        .parse::<u32>()
        .ok()
        .or_else(|| digits.bytes().all(|b| b == b'0').then_some(0))?;
    u16::try_from(value).ok()
}

/// Forbidden domain code points of the WHATWG URL standard, ASCII part.
const FORBIDDEN_DOMAIN: &[u8] = b"#%/:<>?@[\\]^|";

/// The host as a WHATWG parser serializes it, or `None` where that needs
/// rules this module does not reproduce.
fn canonical_host(host: &str) -> Option<String> {
    if let Some(inner) = host.strip_prefix('[') {
        return canonical_ipv6(inner.strip_suffix(']')?).map(|v6| format!("[{v6}]"));
    }
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_graphic() && !FORBIDDEN_DOMAIN.contains(&b))
    {
        return None;
    }
    let host = host.to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    if labels.iter().any(|label| label.starts_with("xn--")) {
        return None;
    }
    if ends_in_a_number(&labels) {
        // Only the form the parser keeps as it is: four plain decimals.
        let plain = labels.len() == 4
            && labels.iter().all(|label| {
                !label.is_empty()
                    && label.bytes().all(|b| b.is_ascii_digit())
                    && (label.len() == 1 || !label.starts_with('0'))
                    && label.parse::<u8>().is_ok()
            });
        return plain.then_some(host);
    }
    Some(host)
}

/// WHATWG "ends in a number": the last label (ignoring one trailing empty
/// label) is all digits, or `0x`/`0X` followed by hex digits.
fn ends_in_a_number(labels: &[&str]) -> bool {
    let mut labels = labels;
    if labels.len() > 1 && labels.last().is_some_and(|last| last.is_empty()) {
        labels = &labels[..labels.len() - 1];
    }
    let Some(last) = labels.last() else {
        return false;
    };
    if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    last.strip_prefix("0x")
        .or_else(|| last.strip_prefix("0X"))
        .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// WHATWG IPv6 parsing (as `std` reads the address) and serialization:
/// lowercase hex, no leading zeros, the first longest run of two or more
/// zero pieces written `::`. An embedded IPv4 tail is refused: the standard
/// and `std` read those differently.
fn canonical_ipv6(text: &str) -> Option<String> {
    if text.contains('.') || text.contains('%') {
        return None;
    }
    let pieces = text.parse::<Ipv6Addr>().ok()?.segments();
    let mut best = (0, 0);
    let mut i = 0;
    while i < 8 {
        let start = i;
        while i < 8 && pieces[i] == 0 {
            i += 1;
        }
        if i - start > best.1 - best.0 {
            best = (start, i);
        }
        i = i.max(start + 1);
    }
    let compress = best.1 - best.0 >= 2;
    let mut out = String::new();
    let mut i = 0;
    while i < 8 {
        if compress && i == best.0 {
            out.push_str(if i == 0 { "::" } else { ":" });
            i = best.1;
            continue;
        }
        out.push_str(&format!("{:x}", pieces[i]));
        if i < 7 {
            out.push(':');
        }
        i += 1;
    }
    Some(out)
}

/// Whether the `url` crate would parse `input` as an absolute URL, for any
/// scheme. `false` also where the exact answer needs IDNA tables (see the
/// module docs), so a `true` is always a URL that crate accepts.
pub fn is_absolute_uri(input: &str) -> bool {
    let trimmed = input.trim_matches(|c: char| c <= ' ');
    let cleaned: String = trimmed
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let Some((scheme, rest)) = cleaned.split_once(':') else {
        return false;
    };
    let mut bytes = scheme.bytes();
    if !bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        || !bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    {
        return false;
    }
    match scheme.to_ascii_lowercase().as_str() {
        "file" => valid_file(rest),
        "http" | "https" | "ws" | "wss" | "ftp" => {
            valid_authority(rest.trim_start_matches(['/', '\\']), true)
        }
        _ => rest
            .strip_prefix("//")
            .is_none_or(|rest| valid_authority(rest, false)),
    }
}

/// The authority of an absolute URL, read the way the `url` crate reads it:
/// userinfo up to the last `@`, the host up to `:` (outside brackets) or the
/// path, then an optional port; whatever follows is the path, which never
/// fails to parse.
fn valid_authority(rest: &str, special: bool) -> bool {
    let ends = |c: char| matches!(c, '/' | '?' | '#') || (special && c == '\\');
    let scanned = rest.find(ends).unwrap_or(rest.len());
    let (has_userinfo, rest) = match rest[..scanned].rfind('@') {
        None => (false, rest),
        Some(0) if rest[1..].starts_with(ends) => return false,
        Some(0) => (false, &rest[1..]),
        Some(at) => (&rest[..at] != ":", &rest[at + 1..]),
    };
    let mut in_brackets = false;
    let mut host_end = rest.len();
    for (index, c) in rest.char_indices() {
        match c {
            ':' if !in_brackets => {}
            c if ends(c) => {}
            '[' => {
                in_brackets = true;
                continue;
            }
            ']' => {
                in_brackets = false;
                continue;
            }
            _ => continue,
        }
        host_end = index;
        break;
    }
    let (host, after) = rest.split_at(host_end);
    let host_ok = if special {
        valid_special_host(host)
    } else {
        valid_opaque_host(host)
    };
    if !host_ok || (host.is_empty() && (has_userinfo || after.starts_with(':'))) {
        return false;
    }
    let Some(port) = after.strip_prefix(':') else {
        return true;
    };
    let digits = port
        .find(['/', '\\', '?', '#'])
        .map_or(port, |end| &port[..end]);
    digits.is_empty() || parse_port(digits).is_some()
}

/// A special-scheme host the WHATWG parser accepts (ASCII hosts only).
fn valid_special_host(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[') {
        return inner.strip_suffix(']').is_some_and(valid_ipv6);
    }
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_graphic() && !FORBIDDEN_DOMAIN.contains(&b))
    {
        return false;
    }
    let host = host.to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    if labels.iter().any(|label| label.starts_with("xn--")) {
        return false;
    }
    !ends_in_a_number(&labels) || valid_ipv4(&labels)
}

/// WHATWG IPv4 parsing of a host that ends in a number.
fn valid_ipv4(labels: &[&str]) -> bool {
    let mut labels = labels.to_vec();
    if labels.len() > 1 && labels.last().is_some_and(|last| last.is_empty()) {
        labels.pop();
    }
    if labels.len() > 4 {
        return false;
    }
    let mut numbers = Vec::new();
    for label in &labels {
        let (digits, radix) = if let Some(hex) = label
            .strip_prefix("0x")
            .or_else(|| label.strip_prefix("0X"))
        {
            (hex, 16)
        } else if label.len() > 1 && label.starts_with('0') {
            (&label[1..], 8)
        } else {
            (*label, 10)
        };
        if label.is_empty() {
            return false;
        }
        let value = if digits.is_empty() {
            Some(0)
        } else if digits.bytes().all(|b| (b as char).is_digit(radix)) {
            u64::from_str_radix(digits, radix).ok().or(Some(u64::MAX))
        } else {
            return false;
        };
        numbers.push(value.unwrap_or(u64::MAX));
    }
    let (last, rest) = match numbers.split_last() {
        Some(split) => split,
        None => return false,
    };
    rest.iter().all(|&n| n <= 255) && *last < 256u64.pow(5 - numbers.len() as u32)
}

fn valid_ipv6(text: &str) -> bool {
    if text.contains('%') {
        return false;
    }
    // `std` agrees with the standard on pure-hex forms; an embedded IPv4
    // tail is accepted only in the strict dotted-decimal form both read.
    match text.rsplit_once(':') {
        Some((_, tail)) if tail.contains('.') => {
            let plain = tail.split('.').count() == 4
                && tail.split('.').all(|part| {
                    !part.is_empty()
                        && part.bytes().all(|b| b.is_ascii_digit())
                        && (part.len() == 1 || !part.starts_with('0'))
                        && part.parse::<u8>().is_ok()
                });
            plain && text.parse::<Ipv6Addr>().is_ok()
        }
        _ => text.parse::<Ipv6Addr>().is_ok(),
    }
}

/// `file:` URLs: a host is only parsed after `//` and may be empty, a
/// Windows drive letter in its place is a path, and a host takes no port.
fn valid_file(rest: &str) -> bool {
    let Some(after) = rest
        .strip_prefix("//")
        .or_else(|| rest.strip_prefix("\\\\"))
        .or_else(|| rest.strip_prefix("/\\"))
        .or_else(|| rest.strip_prefix("\\/"))
    else {
        return true;
    };
    let end = after.find(['/', '\\', '?', '#']).unwrap_or(after.len());
    let host = &after[..end];
    let drive = host.len() == 2
        && host.as_bytes()[0].is_ascii_alphabetic()
        && matches!(host.as_bytes()[1], b':' | b'|');
    host.is_empty() || drive || valid_special_host(host)
}

/// An opaque host (non-special schemes): an IPv6 literal, or any text
/// without a forbidden host code point. Empty is allowed.
fn valid_opaque_host(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[') {
        return inner.strip_suffix(']').is_some_and(valid_ipv6);
    }
    !host.contains([
        '\0', '\t', '\n', '\r', ' ', '#', '/', ':', '<', '>', '?', '@', '[', '\\', ']', '^', '|',
    ])
}

#[cfg(test)]
mod tests {
    use super::{is_absolute_uri, WebUrl};

    /// Input and the serialization url 2.5.8 gives it, or `None` where
    /// this parser refuses it (generated with that crate, then frozen).
    const STRICT: &[(&str, Option<&str>)] = &[
        (
            "https://huggingface.co/org/repo/resolve/main/model.Q4_K_M.gguf?download=true",
            Some("https://huggingface.co/org/repo/resolve/main/model.Q4_K_M.gguf?download=true"),
        ),
        (
            "HTTPS://HuggingFace.CO:443/a/b",
            Some("https://huggingface.co/a/b"),
        ),
        ("http://example.com:080", Some("http://example.com/")),
        (
            "http://example.com:8080/x?q=1#frag",
            Some("http://example.com:8080/x?q=1#frag"),
        ),
        ("http://[0:0:0:0:0:0:0:1]:9090/", Some("http://[::1]:9090/")),
        ("http://[1:0:0:2:0:0:0:3]/", Some("http://[1:0:0:2::3]/")),
        ("http://8.8.8.8/", Some("http://8.8.8.8/")),
        ("https://a.b./%20x/", Some("https://a.b./%20x/")),
        ("https://h/a/%2e/b", None),
        ("https://h/a/../b", None),
        ("https://h/a b", None),
        ("https://user@h/", None),
        ("https://bücher.de/", None),
        ("https://xn--bcher-kva.de/", None),
        ("https://0x7f.1/", None),
        ("https://1.2.3/", None),
        ("https://h:65536/", None),
        ("https://h/a\\b", None),
        ("ftp://h/", None),
        ("https:h/x", None),
        (" https://h/", None),
        ("https://h/?a'b", None),
        ("https://h/#a`b", None),
        ("https://h/a{b}", None),
        ("https://[::ffff:1.2.3.4]/", None),
        ("https://h/a/", Some("https://h/a/")),
    ];

    /// Input and the answer; equal to `url::Url::parse(..).is_ok()` except
    /// on the marked rows.
    const VALID: &[(&str, bool)] = &[
        ("https://example.com/resource", true),
        ("relative/path", false),
        ("", false),
        ("file:///x", true),
        ("file://localhost/x", true),
        ("file://C:/x", true),
        ("file://h:1/x", false),
        ("data:image/png;base64,AAAA", true),
        ("mailto:a@b", true),
        ("urn:isbn:0451450523", true),
        ("foo://host:8080/x", true),
        ("foo://:8080/x", false),
        ("foo://user@/x", false),
        ("foo://h^/x", false),
        ("foo://bü/x", true),
        ("http://", false),
        ("http:///x", true),
        ("http:example.com", true),
        ("http://a b/", false),
        ("http://1.2.3.256/", false),
        ("http://0x1.0x1.1/", true),
        ("http://4294967296/", false),
        ("http://[::1]:99999/", false),
        ("http://[1::2::3]/", false),
        ("  https://e.com  ", true),
        ("ht\ttps://e.com", true),
        ("1http://x", false),
        ("a+b.c-d:x", true),
        ("ws://h:/", true),
        ("https://user:pw@h:443/", true),
        ("https://@h/", true),
        ("https://@/", false),
        ("https://bücher.de/", false), // url accepts it; the IDNA case is refused
        ("https://%41.com/", false),   // url accepts it; the IDNA case is refused
    ];

    #[test]
    fn strict_parse_matches_the_url_crate_or_refuses() {
        for (input, expected) in STRICT {
            let parsed = WebUrl::parse(input).ok();
            assert_eq!(parsed.as_ref().map(WebUrl::as_str), *expected, "{input:?}");
        }
    }

    #[test]
    fn parts_and_origin() {
        let url = WebUrl::parse("HTTPS://Hf.CO:443/org/repo/resolve/main/m.gguf?download=true#x")
            .expect("parses");
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), "hf.co");
        assert_eq!(url.port(), None);
        assert_eq!(url.port_or_known_default(), 443);
        assert_eq!(url.path(), "/org/repo/resolve/main/m.gguf");
        let segments: Vec<&str> = url.path_segments().collect();
        assert_eq!(segments, ["org", "repo", "resolve", "main", "m.gguf"]);
        assert_eq!(url.query(), Some("download=true"));
        assert_eq!(url.fragment(), Some("x"));
        let base = WebUrl::parse("https://hf.co").expect("parses");
        assert_eq!(url.origin(), base.origin());
        assert_ne!(
            url.origin(),
            WebUrl::parse("https://hf.co:8443/")
                .expect("parses")
                .origin()
        );
        assert_ne!(
            url.origin(),
            WebUrl::parse("http://hf.co/").expect("parses").origin()
        );
        let v6 = WebUrl::parse("http://[::1]:8080/").expect("parses");
        assert_eq!((v6.host_str(), v6.port()), ("[::1]", Some(8080)));
    }

    #[test]
    fn absolute_uri_matches_the_url_crate_except_idna_hosts() {
        for (input, expected) in VALID {
            assert_eq!(is_absolute_uri(input), *expected, "{input:?}");
        }
    }
}
