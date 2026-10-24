//! Just enough HTTP/1.1 for the front of a mock server and for the console:
//! read one request, read a response's head and body, write a response.
//!
//! One request per connection, answered with `Connection: close`. A front that
//! relays a backend's response as it arrives cannot know where it ends
//! without reading it all, which an event stream never lets it do; closing is
//! the one framing every client understands.

use std::io::{self, BufRead, Read, Write};

/// The most a request or response head may be.
const MAX_HEAD: usize = 64 * 1024;
/// The most a request body may be. Breeze Core's own largest is a program.
pub const MAX_BODY: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// Path and query, as sent.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or("")
    }

    pub fn query(&self) -> Option<&str> {
        self.target.split_once('?').map(|(_, q)| q)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    pub fn remove_header(&mut self, name: &str) {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    pub fn set_header(&mut self, name: &str, value: &str) {
        self.remove_header(name);
        self.headers.push((name.to_string(), value.to_string()));
    }

    /// The host the client asked for, without a port, lower-cased.
    pub fn host(&self) -> Option<String> {
        let h = self.header("host")?.trim();
        let h = match h.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(v6),
            None => h.rsplit_once(':').map(|(n, _)| n).unwrap_or(h),
        };
        Some(h.to_ascii_lowercase())
    }

    /// The request as bytes, with a `Content-Length` that matches its body.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("{} {} HTTP/1.1\r\n", self.method, self.target).into_bytes();
        for (n, v) in &self.headers {
            if n.eq_ignore_ascii_case("content-length") || n.eq_ignore_ascii_case("transfer-encoding") {
                continue;
            }
            out.extend_from_slice(format!("{n}: {v}\r\n").as_bytes());
        }
        if !self.body.is_empty() || matches!(self.method.as_str(), "POST" | "PUT" | "PATCH") {
            out.extend_from_slice(format!("Content-Length: {}\r\n", self.body.len()).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// Lines up to the blank one, without their CRLF. `None` at a clean EOF before
/// anything arrived.
fn read_head(r: &mut impl BufRead) -> io::Result<Option<Vec<String>>> {
    let mut lines = Vec::new();
    let mut total = 0;
    loop {
        let mut line = Vec::new();
        let n = r.take((MAX_HEAD - total + 1) as u64).read_until(b'\n', &mut line)?;
        if n == 0 {
            return if lines.is_empty() && total == 0 {
                Ok(None)
            } else {
                Err(bad("the connection closed in the middle of a head"))
            };
        }
        total += n;
        if total > MAX_HEAD {
            return Err(bad("head too large"));
        }
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        if line.is_empty() {
            if lines.is_empty() {
                continue; // stray CRLF before a request, which RFC 9112 allows
            }
            return Ok(Some(lines));
        }
        lines.push(String::from_utf8(line).map_err(|_| bad("head is not UTF-8"))?);
    }
}

fn parse_headers(lines: &[String]) -> io::Result<Vec<(String, String)>> {
    lines
        .iter()
        .map(|l| {
            let (n, v) = l.split_once(':').ok_or_else(|| bad(format!("bad header line {l:?}")))?;
            Ok((n.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// One request, or `None` if the client closed before sending one.
pub fn read_request(r: &mut impl BufRead) -> io::Result<Option<Request>> {
    let Some(lines) = read_head(r)? else {
        return Ok(None);
    };
    let mut first = lines[0].split(' ');
    let (Some(method), Some(target), Some(version)) = (first.next(), first.next(), first.next())
    else {
        return Err(bad(format!("bad request line {:?}", lines[0])));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(bad(format!("unsupported {version}")));
    }
    let headers = parse_headers(&lines[1..])?;
    let body = read_body(&headers, r, false)?;
    Ok(Some(Request {
        method: method.to_string(),
        target: target.to_string(),
        headers,
        body,
    }))
}

/// A body framed by `Content-Length` or chunking; for a response, otherwise
/// whatever arrives until the connection closes.
pub fn read_body(headers: &[(String, String)], r: &mut impl BufRead, response: bool) -> io::Result<Vec<u8>> {
    if header(headers, "transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        return read_chunked(r);
    }
    if let Some(len) = header(headers, "content-length") {
        let len: usize = len.parse().map_err(|_| bad("bad Content-Length"))?;
        if len > MAX_BODY {
            return Err(bad("body too large"));
        }
        let mut body = vec![0; len];
        r.read_exact(&mut body)?;
        return Ok(body);
    }
    if response {
        let mut body = Vec::new();
        r.take(MAX_BODY as u64 + 1).read_to_end(&mut body)?;
        if body.len() > MAX_BODY {
            return Err(bad("body too large"));
        }
        return Ok(body);
    }
    Ok(Vec::new())
}

fn read_chunked(r: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let mut size = String::new();
        r.read_line(&mut size)?;
        let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or(""), 16)
            .map_err(|_| bad("bad chunk size"))?;
        if size == 0 {
            // Trailers, up to the blank line.
            loop {
                let mut t = String::new();
                if r.read_line(&mut t)? == 0 || t.trim().is_empty() {
                    return Ok(body);
                }
            }
        }
        if body.len() + size > MAX_BODY {
            return Err(bad("body too large"));
        }
        let start = body.len();
        body.resize(start + size, 0);
        r.read_exact(&mut body[start..])?;
        let mut crlf = [0u8; 2];
        r.read_exact(&mut crlf)?;
    }
}

#[derive(Debug, Clone)]
pub struct ResponseHead {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
}

impl ResponseHead {
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    pub fn remove_header(&mut self, name: &str) {
        self.headers.retain(|(n, _)| !n.eq_ignore_ascii_case(name));
    }

    pub fn set_header(&mut self, name: &str, value: &str) {
        self.remove_header(name);
        self.headers.push((name.to_string(), value.to_string()));
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("HTTP/1.1 {} {}\r\n", self.status, self.reason).into_bytes();
        for (n, v) in &self.headers {
            out.extend_from_slice(format!("{n}: {v}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out
    }
}

pub fn read_response_head(r: &mut impl BufRead) -> io::Result<ResponseHead> {
    let lines = read_head(r)?.ok_or_else(|| bad("no response"))?;
    let mut first = lines[0].splitn(3, ' ');
    let (_, Some(status)) = (first.next(), first.next()) else {
        return Err(bad(format!("bad status line {:?}", lines[0])));
    };
    let status: u16 = status.parse().map_err(|_| bad("bad status code"))?;
    let reason = first.next().unwrap_or("").to_string();
    Ok(ResponseHead {
        status,
        reason,
        headers: parse_headers(&lines[1..])?,
    })
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        421 => "Misdirected Request",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

/// A complete response, closed after.
pub fn respond<W: Write + ?Sized>(w: &mut W, status: u16, content_type: &str, body: &[u8]) -> io::Result<()> {
    let head = ResponseHead {
        status,
        reason: reason(status).to_string(),
        headers: vec![
            ("Content-Type".into(), content_type.into()),
            ("Content-Length".into(), body.len().to_string()),
            ("Connection".into(), "close".into()),
        ],
    };
    w.write_all(&head.to_bytes())?;
    w.write_all(body)?;
    w.flush()
}

/// `{"detail": message}`, the shape every Breeze Core error has.
pub fn detail<W: Write + ?Sized>(w: &mut W, status: u16, message: &str) -> io::Result<()> {
    let body = serde_json::json!({ "detail": message }).to_string();
    respond(w, status, "application/json", body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_with_a_body_and_a_host() {
        let raw = b"POST /api/units/1/control?x=1 HTTP/1.1\r\nHost: Breeze.Test:8443\r\nContent-Length: 4\r\n\r\nbody";
        let r = read_request(&mut &raw[..]).unwrap().unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path(), "/api/units/1/control");
        assert_eq!(r.query(), Some("x=1"));
        assert_eq!(r.host().as_deref(), Some("breeze.test"));
        assert_eq!(r.body, b"body");
    }

    #[test]
    fn chunked_bodies_and_bracketed_hosts() {
        let raw = b"PUT /x HTTP/1.1\r\nHost: [::1]:8420\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n";
        let r = read_request(&mut &raw[..]).unwrap().unwrap();
        assert_eq!(r.body, b"abcde");
        assert_eq!(r.host().as_deref(), Some("::1"));
    }

    #[test]
    fn an_empty_connection_is_not_an_error() {
        assert!(read_request(&mut &b""[..]).unwrap().is_none());
        assert!(read_request(&mut &b"GET / HTTP/1.1\r\nHost"[..]).is_err());
    }

    #[test]
    fn a_rewritten_request_carries_a_matching_length() {
        let raw = b"POST /a HTTP/1.1\r\nContent-Length: 2\r\n\r\nab";
        let mut r = read_request(&mut &raw[..]).unwrap().unwrap();
        r.body = b"abcdef".to_vec();
        let again = read_request(&mut &r.to_bytes()[..]).unwrap().unwrap();
        assert_eq!(again.body, b"abcdef");
    }
}
