//! The panel's HTTP (design note 2026-10-31-lords-panel-remote, D3): just enough HTTP/1.1 for our own client and our
//! own server, over std's TCP. One request per connection (`Connection: close`), bodies by `Content-Length` only, JSON
//! both ways.
//!
//! The server listens on 127.0.0.1 alone, and answers only a request that carries the panel's token (review r1 R2:
//! loopback is open to every local account, and to every account of the PC a forward runs on); without it, 401 and
//! nothing else. Of the rest, only what no browser page can send: a `Host` of 127.0.0.1 (a rebound DNS name is refused),
//! no `Origin`, `Referer` or `Sec-Fetch-*` header (every current browser sends one of them), and a POST only as
//! `application/json` (which a page can't send to another site without a preflight, and the preflight's OPTIONS is
//! refused). Each limit is checked before what it limits is read, and a request has one deadline from its accept to its
//! last byte, however it trickles (review r1 R5).

use serde::Serialize;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A request's line and headers, at most.
pub const MAX_HEAD: usize = 16 * 1024;
/// A request's body, at most: an entry's text with room to spare.
pub const MAX_BODY: usize = 4 * 1024 * 1024;
/// A reply's body, at most, as the client reads it.
pub const MAX_REPLY: usize = 256 * 1024 * 1024;
/// How long the server waits for a client to take its reply.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// The header that carries the panel's token.
pub const TOKEN_HEADER: &str = "Lords-Panel-Token";

/// What the server takes from its clients, at most: each a setting, these its defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// From a connection's accept to its request's last byte, however the bytes come; then the request is refused
    /// (408) and its connection's place freed.
    pub request: Duration,
    /// Connections served at once; past them a new one is told to come back.
    pub connections: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { request: Duration::from_secs(60), connections: 16 }
    }
}

/// The panel's capability: every request carries it, reads and writes alike. Shown nowhere: its `Debug` says only
/// that it is one.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(…)")
    }
}

impl Token {
    /// A token as its file holds it: hex digits, at least 32.
    pub fn new(text: &str) -> Result<Token, String> {
        let t = text.trim();
        if t.len() >= 32 && t.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(Token(t.to_string()))
        } else {
            Err("the panel's token file holds no token".into())
        }
    }

    /// Its text, for the one place it travels: `lords panel add`'s copy to the other PC, over ssh. Never printed.
    pub fn secret(&self) -> &str {
        &self.0
    }

    /// Whether `other` is this token, by `subtle`'s constant-time comparison: its time doesn't depend on where they
    /// differ, only on their lengths (fix check r1).
    fn is(&self, other: &str) -> bool {
        use subtle::ConstantTimeEq;
        bool::from(self.0.as_bytes().ct_eq(other.as_bytes()))
    }
}
/// Headers only a browser sends.
const BROWSER_HEADERS: [&str; 6] =
    ["origin", "referer", "sec-fetch-site", "sec-fetch-mode", "sec-fetch-dest", "sec-fetch-user"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json<T: Serialize>(value: &T) -> Reply {
        match serde_json::to_vec(value) {
            Ok(body) => Reply { status: 200, body },
            Err(e) => Reply::error(500, format!("the reply doesn't serialise: {e}")),
        }
    }

    /// `{"error": message}` with this status.
    pub fn error(status: u16, message: impl Into<String>) -> Reply {
        let body = serde_json::json!({ "error": message.into() });
        Reply { status, body: body.to_string().into_bytes() }
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

// ---------------------------------------------------------------- server

/// A listener on 127.0.0.1 alone: nothing outside this PC reaches it but through an SSH forward.
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port))
}

/// Serves `listener` until the process ends: each connection on its own thread, one request each, `handle` called
/// only for a request that carries `token` and passed every check, within `limits`. A client that goes away mid-reply
/// changes nothing: `handle` has run whole by then.
pub fn serve(
    listener: TcpListener,
    limits: Limits,
    token: Token,
    handle: impl Fn(&Request) -> Reply + Send + Sync + 'static,
) {
    let (handle, token) = (Arc::new(handle), Arc::new(token));
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let until = Instant::now() + limits.request;
        if open.fetch_add(1, Ordering::SeqCst) >= limits.connections {
            open.fetch_sub(1, Ordering::SeqCst);
            let _ = write_reply(&mut stream, &Reply::error(503, "busy: too many connections"));
            continue;
        }
        let (handle, token, open) = (handle.clone(), token.clone(), open.clone());
        std::thread::spawn(move || {
            let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
            let reply = match read_request(&mut stream, until, &token) {
                Ok(request) => handle(&request),
                Err(refused) => refused,
            };
            let _ = write_reply(&mut stream, &reply);
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

fn write_reply(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        reply.status,
        reason(reply.status),
        reply.body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(&reply.body)?;
    stream.flush()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Whether a `Host` value names 127.0.0.1, with a port or without.
fn loopback_host(value: &str) -> bool {
    match value.strip_prefix("127.0.0.1") {
        Some("") => true,
        Some(rest) => rest.strip_prefix(':').is_some_and(|p| {
            !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit())
        }),
        None => false,
    }
}

/// Reads what `stream` has into `buf`, by `until` at the latest: 0 at its end, or the reply that refuses a request
/// past its deadline (review r1 R5: a read that waits on each next byte alone lets a trickle hold its place for ever).
fn read_by(stream: &mut TcpStream, buf: &mut [u8], until: Instant) -> Result<usize, Reply> {
    let late = || Reply::error(408, "the request wasn't sent in time");
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(late());
    }
    stream.set_read_timeout(Some(left)).map_err(|e| Reply::error(400, e.to_string()))?;
    match stream.read(buf) {
        Ok(n) => Ok(n),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Err(late())
        }
        Err(e) => Err(Reply::error(400, format!("the request couldn't be read: {e}"))),
    }
}

/// One request off `stream`, all of it by `until`, carrying `token`; or the reply that refuses it.
pub fn read_request(
    stream: &mut TcpStream,
    until: Instant,
    token: &Token,
) -> Result<Request, Reply> {
    let bad = |m: &str| Reply::error(400, m);
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    let end = loop {
        if let Some(end) = find(&head, b"\r\n\r\n") {
            break end;
        }
        if head.len() > MAX_HEAD {
            return Err(Reply::error(431, format!("the request's head is over {MAX_HEAD} bytes")));
        }
        match read_by(stream, &mut buf, until)? {
            0 => return Err(bad("the request ended before its head did")),
            n => head.extend_from_slice(&buf[..n]),
        }
    };
    if end > MAX_HEAD {
        return Err(Reply::error(431, format!("the request's head is over {MAX_HEAD} bytes")));
    }
    let mut body = head.split_off(end + 4);
    let text =
        std::str::from_utf8(&head[..end]).map_err(|_| bad("the request's head isn't UTF-8"))?;
    let mut lines = text.split("\r\n");
    let line: Vec<&str> = lines.next().unwrap_or("").split(' ').collect();
    let &[method, target, version] = line.as_slice() else {
        return Err(bad("a bad request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(bad("only HTTP/1.x"));
    }
    if method != "GET" && method != "POST" {
        return Err(Reply::error(405, format!("{method} isn't answered here")));
    }
    let mut headers: Vec<(String, String)> = Vec::new();
    for l in lines {
        let (name, value) = l.split_once(':').ok_or_else(|| bad("a bad header line"))?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    fn all<'a>(headers: &'a [(String, String)], name: &str) -> Vec<&'a str> {
        headers.iter().filter(|(n, _)| n == name).map(|(_, v)| v.as_str()).collect()
    }
    let one = |name: &str| -> Result<Option<&str>, Reply> {
        match all(&headers, name)[..] {
            [] => Ok(None),
            [v] => Ok(Some(v)),
            _ => Err(bad(&format!("more than one {name}"))),
        }
    };
    // Before anything else is read or said: without the token, once, 401 and nothing else.
    let carried = all(&headers, &TOKEN_HEADER.to_ascii_lowercase());
    if !matches!(carried[..], [t] if token.is(t)) {
        return Err(Reply { status: 401, body: Vec::new() });
    }
    if !one("host")?.is_some_and(loopback_host) {
        return Err(Reply::error(403, "only requests to 127.0.0.1 are answered"));
    }
    if let Some(h) = BROWSER_HEADERS.iter().find(|h| !all(&headers, h).is_empty()) {
        return Err(Reply::error(403, format!("a browser's request ({h}) is refused")));
    }
    if !all(&headers, "transfer-encoding").is_empty() {
        return Err(bad("bodies come with a Content-Length only"));
    }
    let length = match one("content-length")? {
        None => None,
        Some(v) if !v.is_empty() && v.len() <= 20 && v.bytes().all(|b| b.is_ascii_digit()) => {
            Some(v.parse::<u64>().map_err(|_| bad("a bad Content-Length"))?)
        }
        Some(_) => return Err(bad("a bad Content-Length")),
    };
    let length = match (method, length) {
        ("GET", None | Some(0)) => 0,
        ("GET", Some(_)) => return Err(bad("a GET has no body")),
        (_, None) => return Err(Reply::error(411, "a POST needs a Content-Length")),
        (_, Some(n)) => {
            let json = one("content-type")?
                .map(|t| t.split(';').next().unwrap_or("").trim().to_ascii_lowercase());
            if json.as_deref() != Some("application/json") {
                return Err(Reply::error(415, "a POST is application/json"));
            }
            if n > MAX_BODY as u64 {
                return Err(Reply::error(413, format!("the body is over {MAX_BODY} bytes")));
            }
            n as usize
        }
    };
    if body.len() > length {
        return Err(bad("more bytes than the Content-Length"));
    }
    let mut have = body.len();
    body.resize(length, 0);
    while have < length {
        match read_by(stream, &mut body[have..], until)? {
            0 => return Err(bad("the body ended early")),
            n => have += n,
        }
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut pairs = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let k = decode(k).ok_or_else(|| bad("a bad query"))?;
        let v = decode(v).ok_or_else(|| bad("a bad query"))?;
        pairs.push((k, v));
    }
    Ok(Request { method: method.to_string(), path: path.to_string(), query: pairs, body })
}

/// Percent-encodes everything but the unreserved characters.
pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

// ---------------------------------------------------------------- client

/// `api`'s port: `http://127.0.0.1:<port>` only (this PC, or an SSH forward).
pub fn api_port(api: &str) -> Result<u16, String> {
    api.trim_end_matches('/')
        .strip_prefix("http://127.0.0.1:")
        .and_then(|p| p.parse::<u16>().ok())
        .filter(|p| *p != 0)
        .ok_or_else(|| "api must be http://127.0.0.1:<port> (this PC, or an SSH forward)".into())
}

/// The panel's server at `http://127.0.0.1:<port>`: this PC's, or another's through an SSH forward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    port: u16,
    token: Token,
}

/// Why a call got no reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// Nothing was sent: the request is past what the server takes.
    TooLarge(String),
    /// The request didn't go out whole: the server never handled it.
    Unreachable(String),
    /// The request went out whole, and then the reply didn't come back whole: whatever it asked for may have happened.
    Broken(String),
}

impl Client {
    /// The server on `port`, reached with its `token`.
    pub fn new(port: u16, token: Token) -> Client {
        Client { port, token }
    }

    /// `api` as config.txt has it: `http://127.0.0.1:<port>`, loopback only (this PC, or an SSH forward).
    pub fn from_api(api: &str, token: Token) -> Result<Client, String> {
        Ok(Client { port: api_port(api)?, token })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// One call, done within `deadline`: the reply's status and body.
    pub fn call(
        &self,
        method: &str,
        target: &str,
        body: Option<&[u8]>,
        deadline: Duration,
    ) -> Result<(u16, Vec<u8>), CallError> {
        // The server refuses it before reading it, and its refusal may then arrive as a reset: say it here.
        if body.is_some_and(|b| b.len() > MAX_BODY) {
            return Err(CallError::TooLarge(format!(
                "the request is over {MAX_BODY} bytes; nothing was sent"
            )));
        }
        let started = Instant::now();
        let left = |at_least: Duration| deadline.saturating_sub(started.elapsed()).max(at_least);
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let unreachable =
            |e: std::io::Error| CallError::Unreachable(format!("127.0.0.1:{}: {e}", self.port));
        let mut stream = TcpStream::connect_timeout(
            &addr,
            left(Duration::ZERO).min(Duration::from_secs(5)).max(Duration::from_millis(1)),
        )
        .map_err(unreachable)?;
        let mut request = format!(
            "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{TOKEN_HEADER}: {}\r\nConnection: close\r\n",
            self.port, self.token.0
        );
        if let Some(b) = body {
            request.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                b.len()
            ));
        }
        request.push_str("\r\n");
        let mut bytes = request.into_bytes();
        bytes.extend_from_slice(body.unwrap_or_default());
        stream.set_write_timeout(Some(left(Duration::from_millis(1)))).map_err(unreachable)?;
        stream.write_all(&bytes).and_then(|()| stream.flush()).map_err(unreachable)?;
        let broken = |m: String| CallError::Broken(format!("127.0.0.1:{}: {m}", self.port));
        let mut reply = Vec::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            if started.elapsed() >= deadline {
                return Err(broken(format!("no reply within {} s", deadline.as_secs())));
            }
            stream
                .set_read_timeout(Some(left(Duration::from_millis(1))))
                .map_err(|e| broken(e.to_string()))?;
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => reply.extend_from_slice(&buf[..n]),
                Err(e) => return Err(broken(e.to_string())),
            }
            if reply.len() > MAX_HEAD + MAX_REPLY {
                return Err(broken(format!("a reply over {MAX_REPLY} bytes")));
            }
        }
        let end = find(&reply, b"\r\n\r\n").ok_or_else(|| broken("a reply with no head".into()))?;
        let head = String::from_utf8_lossy(&reply[..end]).into_owned();
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| broken("a reply with no status".into()))?;
        let length = head
            .split("\r\n")
            .filter_map(|l| l.split_once(':'))
            .find(|(n, _)| n.trim().eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .ok_or_else(|| broken("a reply with no Content-Length".into()))?;
        let body = reply.split_off(end + 4);
        if body.len() != length {
            return Err(broken(format!("a reply of {} bytes, not {length}", body.len())));
        }
        Ok((status, body))
    }
}

/// `path?k=v&…`, each value percent-encoded.
pub fn target(path: &str, params: &[(&str, &str)]) -> String {
    let query: Vec<String> = params.iter().map(|(k, v)| format!("{k}={}", encode(v))).collect();
    if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", query.join("&"))
    }
}
