//! The front of the mock servers: listeners, TLS and virtual hosts, faults and
//! latency, the release profile, the pairing policy -- and then the request,
//! as Breeze Core would receive it from a reverse proxy, to the real Breeze
//! Core behind.

use std::io::{self, BufReader, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::backend::Backend;
use super::http::{self, Request};
use super::profile;
use crate::blueprint::{Approval, Fault, Server};
use crate::sim::rng::Rng;
use crate::{debug, error, info};

/// One mock server: what the blueprint says, and the Breeze Core behind it.
pub struct MockServer {
    pub def: Server,
    pub backend: Backend,
    rng: Mutex<Rng>,
    /// Itself, for the threads it starts.
    me: std::sync::Weak<MockServer>,
    /// Pairing codes issued and not yet approved or expired, for the console.
    pub pending: Mutex<Vec<(String, String, Instant)>>,
    /// Requests answered, and faults injected.
    pub stats: Mutex<FrontStats>,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct FrontStats {
    pub requests: u64,
    pub faults: u64,
    pub auto_approved: u64,
}

impl MockServer {
    pub fn new(def: Server, backend: Backend, seed: u64) -> Arc<Self> {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in def.name.bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
        Arc::new_cyclic(|me| Self {
            def,
            backend,
            rng: Mutex::new(Rng::new(seed ^ h)),
            me: me.clone(),
            pending: Mutex::new(Vec::new()),
            stats: Mutex::new(FrontStats::default()),
        })
    }

    fn self_ref(&self) -> std::sync::Weak<MockServer> {
        self.me.clone()
    }

    /// Approve a pairing code as an administrator on the LAN would.
    pub fn approve(&self, code: &str) -> Result<(), String> {
        let body = serde_json::json!({ "code": code }).to_string();
        let mut req = Request {
            method: "POST".into(),
            target: "/api/auth/enroll/approve".into(),
            headers: vec![
                ("Host".into(), "localhost".into()),
                ("X-API-Key".into(), self.def.api_key.clone()),
                ("Content-Type".into(), "application/json".into()),
                ("X-Forwarded-For".into(), "127.0.0.1".into()),
                ("Connection".into(), "close".into()),
            ],
            body: body.into_bytes(),
        };
        req.remove_header("accept-encoding");
        let (status, body) = self.backend_call(&req)?;
        if status == 200 {
            if let Ok(mut p) = self.pending.lock() {
                p.retain(|(c, _, _)| c != code);
            }
            Ok(())
        } else {
            Err(format!("Breeze Core said {status}: {}", String::from_utf8_lossy(&body)))
        }
    }

    /// One complete request to the backend, for the front's own use.
    fn backend_call(&self, req: &Request) -> Result<(u16, Vec<u8>), String> {
        let mut s = TcpStream::connect_timeout(&self.backend.addr, Duration::from_secs(5))
            .map_err(|e| e.to_string())?;
        s.write_all(&req.to_bytes()).map_err(|e| e.to_string())?;
        let mut r = BufReader::new(s);
        let head = http::read_response_head(&mut r).map_err(|e| e.to_string())?;
        let body = http::read_body(&head.headers, &mut r, true).map_err(|e| e.to_string())?;
        Ok((head.status, body))
    }

    pub fn pending_codes(&self) -> Vec<(String, String)> {
        let ttl = Duration::from_secs(self.def.pairing.code_ttl);
        let Ok(mut p) = self.pending.lock() else { return Vec::new() };
        p.retain(|(_, _, at)| at.elapsed() < ttl);
        p.iter().map(|(c, l, _)| (c.clone(), l.clone())).collect()
    }
}

/// A socket the front listens on, and the servers that share it.
pub struct Listener {
    pub addr: SocketAddr,
    pub tls: Option<Arc<rustls::ServerConfig>>,
    pub servers: Vec<Arc<MockServer>>,
}

trait Conn: Read + Write + Send {}
impl<T: Read + Write + Send> Conn for T {}

pub fn serve(listener: Listener) -> Result<(), String> {
    let socket = TcpListener::bind(listener.addr)
        .map_err(|e| format!("cannot listen on {}: {e}", listener.addr))?;
    let names: Vec<&str> = listener.servers.iter().map(|s| s.def.name.as_str()).collect();
    info!(
        "front",
        "{}://{} serves {}",
        if listener.tls.is_some() { "https" } else { "http" },
        listener.addr,
        names.join(", ")
    );
    let listener = Arc::new(listener);
    std::thread::spawn(move || {
        for stream in socket.incoming() {
            let Ok(stream) = stream else { continue };
            let listener = Arc::clone(&listener);
            std::thread::spawn(move || {
                let peer = stream.peer_addr().ok();
                if let Err(e) = connection(&listener, stream) {
                    debug!("front", "connection from {peer:?} ended: {e}");
                }
            });
        }
    });
    Ok(())
}

fn connection(listener: &Listener, stream: TcpStream) -> io::Result<()> {
    let peer = stream.peer_addr()?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let (mut conn, sni): (Box<dyn Conn>, Option<String>) = match &listener.tls {
        None => (Box::new(stream), None),
        Some(config) => {
            let mut tls = rustls::ServerConnection::new(Arc::clone(config)).map_err(io::Error::other)?;
            let mut stream = stream;
            while tls.is_handshaking() {
                tls.complete_io(&mut stream)?;
            }
            let sni = tls.server_name().map(str::to_ascii_lowercase);
            (Box::new(rustls::StreamOwned::new(tls, stream)), sni)
        }
    };
    let mut reader = BufReader::new(&mut conn);
    let read = http::read_request(&mut reader);
    if read.is_err() {
        let _ = http::detail(reader.get_mut(), 400, "malformed request");
    }
    let Some(req) = read?
    else {
        return Ok(());
    };
    drop(reader);
    let server = match pick(listener, &req, sni.as_deref()) {
        Some(s) => s,
        None => {
            return http::detail(
                &mut conn,
                421,
                "this address serves several domains, and not the one asked for",
            )
        }
    };
    let listen = listener.addr.to_string();
    handle(server, &listen, req, peer, &mut conn)
}

/// The server a request is for: the only one on the socket, or the one whose
/// domain the client named.
fn pick<'a>(listener: &'a Listener, req: &Request, sni: Option<&str>) -> Option<&'a Arc<MockServer>> {
    if listener.servers.len() == 1 {
        return listener.servers.first();
    }
    let host = req.host();
    let wanted = host.as_deref().or(sni)?;
    listener
        .servers
        .iter()
        .find(|s| s.def.domains.iter().any(|d| d.name.eq_ignore_ascii_case(wanted)))
}

fn matches(f: &Fault, req: &Request) -> bool {
    if f.method.as_deref().is_some_and(|m| !m.eq_ignore_ascii_case(&req.method)) {
        return false;
    }
    match f.path.strip_suffix('*') {
        Some(prefix) => req.path().starts_with(prefix),
        None => req.path() == f.path,
    }
}

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
    "x-forwarded-for",
    "x-real-ip",
    "forwarded",
];

fn handle(server: &MockServer, listen: &str, mut req: Request, peer: SocketAddr, out: &mut dyn Conn) -> io::Result<()> {
    let started = Instant::now();
    let who = server.def.name.clone();
    if let Ok(mut s) = server.stats.lock() {
        s.requests += 1;
    }

    // Faults first: they stand in for a network or a server that misbehaves,
    // which happens before anything else does.
    for f in &server.def.faults {
        let hit = matches(f, &req) && server.rng.lock().map(|mut r| r.chance(f.rate)).unwrap_or(false);
        if !hit {
            continue;
        }
        if let Ok(mut s) = server.stats.lock() {
            s.faults += 1;
        }
        if !f.delay.is_zero() {
            std::thread::sleep(f.delay);
        }
        if f.hang_up {
            info!(who, "fault: hanging up on {} {}", req.method, req.target);
            return Ok(());
        }
        info!(who, "fault: {} for {} {}", f.status, req.method, req.target);
        let body = f
            .body
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "detail": http::reason(f.status) }).to_string());
        return http::respond(out, f.status, "application/json", body.as_bytes());
    }
    if !server.def.latency.is_zero() {
        std::thread::sleep(server.def.latency);
    }

    let build = &server.def.build;
    if !profile::has_route(build, &req.method, req.path()) {
        debug!(who, "{} {}: not in {} -> 404", req.method, req.target, build.version);
        return http::detail(out, 404, "Not Found");
    }
    if req.path() == "/api/auth/enroll/approve" && server.def.pairing.approve == Approval::Never {
        return http::detail(out, 403, "this server never approves a pairing (blueprint: approve=\"never\")");
    }
    if let Err((status, message)) = profile::rewrite_request(build, &mut req) {
        return http::detail(out, status, &message);
    }

    // As a reverse proxy would hand it on: the client's own address, which is
    // what Breeze Core's "from the LAN" rules judge -- unless the blueprint
    // lifts them, in which case every client is local.
    let enroll_start = req.method == "POST" && req.path() == "/api/auth/enroll/start";
    let rewrite = profile::rewrites_response(build, &req) || enroll_start;
    for h in HOP_BY_HOP {
        req.remove_header(h);
    }
    let client = if server.def.pairing.lan_only { peer.ip() } else { IpAddr::from([127, 0, 0, 1]) };
    req.set_header("X-Forwarded-For", &client.to_string());
    req.set_header("Connection", "close");
    if rewrite {
        req.remove_header("accept-encoding");
    }

    let backend = match TcpStream::connect_timeout(&server.backend.addr, Duration::from_secs(5)) {
        Ok(s) => s,
        Err(e) => {
            error!(who, "cannot reach its Breeze Core: {e}");
            return http::detail(out, 502, "the Breeze Core behind this mock server is not answering");
        }
    };
    backend.set_nodelay(true)?;
    let mut to_backend = backend.try_clone()?;
    to_backend.write_all(&req.to_bytes())?;
    let mut from = BufReader::new(backend);
    let mut head = http::read_response_head(&mut from)?;
    head.set_header("Connection", "close");

    if rewrite && (200..300).contains(&head.status) {
        let body = http::read_body(&head.headers, &mut from, true)?;
        let mut body = match serde_json::from_slice::<Value>(&body) {
            Ok(v) => v,
            Err(_) => {
                head.remove_header("transfer-encoding");
                head.set_header("Content-Length", &body.len().to_string());
                out.write_all(&head.to_bytes())?;
                out.write_all(&body)?;
                return out.flush();
            }
        };
        if enroll_start {
            let label = serde_json::from_slice::<Value>(&req.body)
                .ok()
                .and_then(|v| v.get("label").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            pairing_started(server, &body, label);
        }
        profile::rewrite_response(&server.def, listen, &req, &mut body);
        let bytes = serde_json::to_vec(&body).unwrap_or_default();
        head.remove_header("transfer-encoding");
        head.remove_header("content-encoding");
        head.set_header("Content-Length", &bytes.len().to_string());
        out.write_all(&head.to_bytes())?;
        out.write_all(&bytes)?;
        out.flush()?;
    } else {
        // As it arrives, flushed each time: the event stream is a response
        // that never ends, and each frame matters the moment it is sent.
        out.write_all(&head.to_bytes())?;
        out.flush()?;
        let mut buf = [0u8; 8192];
        loop {
            let n = from.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            out.flush()?;
        }
    }
    debug!(
        who,
        "{} {} {} -> {} ({} ms)",
        peer.ip(),
        req.method,
        req.target,
        head.status,
        started.elapsed().as_millis()
    );
    Ok(())
}

/// A pairing was started: remember its code for the console, and approve it
/// at once if the blueprint says every code is.
fn pairing_started(server: &MockServer, body: &Value, label: String) {
    let Some(code) = body.get("user_code").and_then(Value::as_str) else {
        return;
    };
    if let Ok(mut p) = server.pending.lock() {
        p.push((code.to_string(), label, Instant::now()));
    }
    let who = server.def.name.clone();
    match server.def.pairing.approve {
        Approval::Auto => {
            // A moment after the code is issued, as an attentive administrator
            // would: a client on the LAN that approves its own code (breezectl
            // does) still gets to, and one that waits sees it approved.
            let code = code.to_string();
            let server = server.self_ref();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                let Some(server) = server.upgrade() else { return };
                match server.approve(&code) {
                    Ok(()) => {
                        info!(who, "pairing code {code} approved automatically");
                        if let Ok(mut s) = server.stats.lock() {
                            s.auto_approved += 1;
                        }
                    }
                    // Already approved by the client itself, or by the console.
                    Err(e) => debug!(who, "pairing code {code} was not left to approve: {e}"),
                }
            });
        }
        Approval::Manual => info!(who, "pairing code {code} is waiting to be approved"),
        Approval::Never => info!(who, "pairing code {code} issued; it will never be approved"),
    }
}
