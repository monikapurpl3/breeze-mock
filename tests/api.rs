//! The running mock, from the outside: what a client of a mock server, a
//! real Breeze Core, or a script poking the console sees.
//!
//! Every test runs its own world on its own loopback addresses and ports, so
//! they run in parallel in one process. The listeners outlive each test; that
//! is fine for a test binary.

use std::io::{BufReader, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use breeze_mock::api::http;
use breeze_mock::blueprint::{self, Blueprint};
use breeze_mock::run::{self, Running};
use rustls::pki_types::pem::PemObject;
use serde_json::{json, Value};

fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("bm-api-data-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("BREEZE_MOCK_DATA", &dir);
        breeze_mock::log::set_level(0);
    });
}

fn load(name: &str, body: &str) -> Blueprint {
    init();
    let dir = std::env::temp_dir().join(format!("bm-api-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.xml"));
    std::fs::write(
        &path,
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <blueprint xmlns=\"urn:breeze-mock:blueprint:1\" version=\"1\" name=\"{name}\" seed=\"5\">\n{body}\n</blueprint>\n"
        ),
    )
    .unwrap();
    blueprint::load(&path).unwrap_or_else(|e| panic!("{e}"))
}

fn start(name: &str, body: &str) -> (Blueprint, Running) {
    let bp = load(name, body);
    let running = run::start(&bp).unwrap_or_else(|e| panic!("{e}"));
    (bp, running)
}

/// Three units on one loopback /24; the third cannot turbo while heating.
fn units(net: &str) -> String {
    format!(
        r#"<environment><constant outdoor="10"/></environment>
  <network><segment name="lo" cidr="{net}.0/24" mode="loopback" discovery="false"/></network>
  <unit key="a" segment="lo" ip="{net}.2">
    <epithets name="Alpha"/><v3 token="auto" key="auto"/>
    <state power="on" mode="cool" target="24"/><behaviour reply-delay="0ms"/>
  </unit>
  <unit key="b" segment="lo" ip="{net}.3">
    <epithets name="Bravo"/><v3 token="auto" key="auto"/>
    <capabilities modes="auto cool heat fan-only" swing="vertical" fan="low high auto" eco="false" turbo="cool"/>
    <state power="on" mode="heat" target="22"/><behaviour reply-delay="0ms"/>
  </unit>"#
    )
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&self.body)))
    }
}

fn call(addr: &str, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Value>) -> Reply {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    for (n, v) in headers {
        req.push_str(&format!("{n}: {v}\r\n"));
    }
    if !body.is_empty() {
        req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    s.write_all(req.as_bytes()).unwrap();
    let mut r = BufReader::new(s);
    let head = http::read_response_head(&mut r).unwrap();
    let body = http::read_body(&head.headers, &mut r, true).unwrap();
    Reply {
        status: head.status,
        body,
    }
}

const TOKEN: &str = "test-device-token-not-a-secret-0123456789";

fn auth(key: &str) -> [(&str, &str); 2] {
    [("X-API-Key", key), ("Authorization", "Bearer test-device-token-not-a-secret-0123456789")]
}

fn server(name: &str, profile: &str, port: u16, extra: &str) -> String {
    format!(
        r#"<server name="{name}" api-key="{name}-key-not-a-secret-01">
    <build profile="{profile}" {extra}/>
    <listen address="127.0.0.1" port="{port}"/>
    <pairing approve="manual"><device label="test" token="{TOKEN}"/></pairing>
    <manage unit="a"/><manage unit="b"/>
  </server>"#
    )
}

fn id_of(addr: &str, key: &str, name: &str) -> String {
    let units = call(addr, "GET", "/api/units", &auth(key), None).json();
    units.as_array().unwrap().iter().find(|u| u["name"] == name).unwrap()["id"].as_str().unwrap().to_string()
}

#[test]
fn a_profile_is_the_release_it_names() {
    let (_bp, _r) = start(
        "profiles",
        &format!(
            "{}\n{}\n{}\n{}",
            units("127.91.1"),
            server("now", "4.3.2", 29101, ""),
            server("before", "4.1.1", 29102, ""),
            server("old", "3.2.0", 29103, "remove=\"programs\""),
        ),
    );
    let now = "127.0.0.1:29101";
    let before = "127.0.0.1:29102";
    let old = "127.0.0.1:29103";

    let v = call(now, "GET", "/api/version", &auth("now-key-not-a-secret-01"), None).json();
    assert_eq!(v["version"], "4.3.2");
    assert_eq!(v["commit"], "2ef0d09");
    assert!(v["features"].as_array().unwrap().contains(&json!("timer_at")));
    let v = call(before, "GET", "/api/version", &auth("before-key-not-a-secret-01"), None).json();
    assert_eq!(v["version"], "4.1.1");
    assert!(!v["features"].as_array().unwrap().contains(&json!("timer_at")));
    let v = call(old, "GET", "/api/version", &auth("old-key-not-a-secret-01"), None).json();
    assert_eq!(v["version"], "3.2.0");
    for f in ["programs", "control_feedback", "timer_at"] {
        assert!(!v["features"].as_array().unwrap().contains(&json!(f)), "3.2.0 minus programs has no {f}");
    }

    // A route the release did not have is a 404, before any authentication.
    assert_eq!(call(old, "GET", "/api/programs", &[], None).status, 404);
    assert_eq!(call(now, "GET", "/api/programs", &auth("now-key-not-a-secret-01"), None).status, 200);

    // not_applied: there with control_feedback, absent before it.
    let b = id_of(now, "now-key-not-a-secret-01", "Bravo");
    let r = call(now, "POST", &format!("/api/units/{b}/control"), &auth("now-key-not-a-secret-01"), Some(json!({"turbo": true}))).json();
    assert_eq!(r["not_applied"], json!(["turbo"]), "Bravo refuses turbo while heating: {r}");
    let r = call(old, "POST", &format!("/api/units/{b}/control"), &auth("old-key-not-a-secret-01"), Some(json!({"turbo": true}))).json();
    assert!(r.get("not_applied").is_none(), "3.2.0 never sent not_applied: {r}");
    assert_eq!(r["turbo"], false);

    // timer_at: a start at a time is refused before it existed -- after auth.
    assert_eq!(
        call(before, "POST", "/api/timers", &[], Some(json!({"unit_ids": [b], "at": "07:00"}))).status,
        401
    );
    let r = call(before, "POST", "/api/timers", &auth("before-key-not-a-secret-01"), Some(json!({"unit_ids": [b], "at": "07:00"})));
    assert_eq!(r.status, 422);
    let r = call(before, "POST", "/api/timers", &auth("before-key-not-a-secret-01"), Some(json!({"unit_ids": [b], "minutes": 5}))).json();
    assert!(r.get("kind").is_none(), "4.1.1 timers have no kind: {r}");
    let r = call(now, "POST", "/api/timers", &auth("now-key-not-a-secret-01"), Some(json!({"unit_ids": [b], "minutes": 5}))).json();
    assert_eq!(r["kind"], "sleep");
}

#[test]
fn faults_answer_for_the_server_or_hang_up() {
    let (_bp, _r) = start(
        "faults",
        &format!(
            r#"{}
  <server name="f" api-key="f-key-not-a-secret-0123">
    <build profile="4.3.2"/>
    <listen address="127.0.0.1" port="29121"/>
    <manage unit="a"/>
    <fault method="GET" path="/api/units*" rate="1" status="503" body="{{&quot;detail&quot;:&quot;nope&quot;}}"/>
    <fault path="/api/version" rate="1" hang-up="true"/>
  </server>"#,
            units("127.91.2")
        ),
    );
    let r = call("127.0.0.1:29121", "GET", "/api/units/state", &[], None);
    assert_eq!((r.status, r.json()), (503, json!({"detail": "nope"})));
    assert_eq!(call("127.0.0.1:29121", "GET", "/api/health", &[], None).status, 200);
    let mut s = TcpStream::connect("127.0.0.1:29121").unwrap();
    s.write_all(b"GET /api/version HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut buf = [0u8; 16];
    assert_eq!(std::io::Read::read(&mut s, &mut buf).unwrap_or(0), 0, "hung up without a word");
}

fn enroll(addr: &str, key: &str) -> (String, String) {
    let r = call(addr, "POST", "/api/auth/enroll/start", &[("X-API-Key", key)], Some(json!({"label": "t", "auth_version": 1}))).json();
    (r["session_id"].as_str().unwrap().into(), r["user_code"].as_str().unwrap().into())
}

fn poll(addr: &str, key: &str, session: &str) -> Value {
    call(addr, "POST", "/api/auth/enroll/poll", &[("X-API-Key", key)], Some(json!({"session_id": session}))).json()
}

#[test]
fn pairing_follows_the_blueprint() {
    let (_bp, running) = start(
        "pairing",
        &format!(
            r#"{}
  <server name="auto" api-key="auto-key-not-a-secret-01"><build profile="4.3.2"/>
    <listen address="127.0.0.1" port="29131"/><pairing approve="auto"/><manage unit="a"/></server>
  <server name="never" api-key="never-key-not-a-secret-01"><build profile="4.3.2"/>
    <listen address="127.0.0.1" port="29132"/><pairing approve="never"/><manage unit="a"/></server>
  <server name="manual" api-key="manual-key-not-a-secret-01"><build profile="4.3.2"/>
    <listen address="127.0.0.1" port="29133"/><manage unit="a"/></server>"#,
            units("127.91.3")
        ),
    );
    // auto: approved by itself, and the token works.
    let (session, _) = enroll("127.0.0.1:29131", "auto-key-not-a-secret-01");
    let deadline = Instant::now() + Duration::from_secs(10);
    let token = loop {
        let p = poll("127.0.0.1:29131", "auto-key-not-a-secret-01", &session);
        if p["status"] == "approved" {
            break p["device_token"].as_str().unwrap().to_string();
        }
        assert!(Instant::now() < deadline, "never approved: {p}");
        std::thread::sleep(Duration::from_millis(200));
    };
    let bearer = format!("Bearer {token}");
    let units = call("127.0.0.1:29131", "GET", "/api/units", &[("X-API-Key", "auto-key-not-a-secret-01"), ("Authorization", &bearer)], None);
    assert_eq!(units.status, 200);

    // never: the approval is refused, and the code stays pending.
    let (session, code) = enroll("127.0.0.1:29132", "never-key-not-a-secret-01");
    let r = call("127.0.0.1:29132", "POST", "/api/auth/enroll/approve", &[("X-API-Key", "never-key-not-a-secret-01")], Some(json!({"code": code})));
    assert_eq!(r.status, 403);
    assert_eq!(poll("127.0.0.1:29132", "never-key-not-a-secret-01", &session)["status"], "pending");

    // manual: waits, and the console's approval is an admin's.
    let (session, code) = enroll("127.0.0.1:29133", "manual-key-not-a-secret-01");
    assert_eq!(poll("127.0.0.1:29133", "manual-key-not-a-secret-01", &session)["status"], "pending");
    let manual = running.servers.iter().find(|s| s.def.name == "manual").unwrap();
    assert_eq!(manual.pending_codes().len(), 1);
    manual.approve(&code).unwrap();
    assert_eq!(poll("127.0.0.1:29133", "manual-key-not-a-secret-01", &session)["status"], "approved");
}

#[test]
fn servers_sharing_a_port_are_told_apart_by_host() {
    let (_bp, _r) = start(
        "vhosts",
        &format!(
            r#"{}
  <server name="one" api-key="one-key-not-a-secret-012"><build profile="4.3.2"/>
    <listen address="127.0.0.1" port="29141"/><domain name="one.test"/><manage unit="a"/></server>
  <server name="two" api-key="two-key-not-a-secret-012"><build profile="4.1.1"/>
    <listen address="127.0.0.1" port="29141"/><domain name="two.test"/><manage unit="a"/></server>"#,
            units("127.91.4")
        ),
    );
    let ask = |host: &str| {
        let mut s = TcpStream::connect("127.0.0.1:29141").unwrap();
        write!(s, "GET /api/health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
        let mut r = BufReader::new(s);
        http::read_response_head(&mut r).unwrap().status
    };
    assert_eq!(ask("one.test:29141"), 200);
    assert_eq!(ask("TWO.test"), 200);
    assert_eq!(ask("three.test"), 421);
    let version = |host: &str, key: &str| {
        let mut s = TcpStream::connect("127.0.0.1:29141").unwrap();
        write!(s, "GET /api/version HTTP/1.1\r\nHost: {host}\r\nX-API-Key: {key}\r\nConnection: close\r\n\r\n").unwrap();
        let mut r = BufReader::new(s);
        let head = http::read_response_head(&mut r).unwrap();
        let body: Value = serde_json::from_slice(&http::read_body(&head.headers, &mut r, true).unwrap()).unwrap();
        body["version"].as_str().unwrap().to_string()
    };
    assert_eq!(version("one.test", "one-key-not-a-secret-012"), "4.3.2");
    assert_eq!(version("two.test", "two-key-not-a-secret-012"), "4.1.1");
}

#[test]
fn tls_by_name_with_the_blueprints_own_authority() {
    let (bp, _r) = start(
        "tls",
        &format!(
            r#"{}
  <server name="s" api-key="s-key-not-a-secret-01234"><build profile="4.3.2"/>
    <listen address="127.0.0.1" port="29151" tls="true"/>
    <domain name="secure.test"><self-signed/></domain><manage unit="a"/></server>"#,
            units("127.91.5")
        ),
    );
    let ca = breeze_mock::api::tls::Ca::load_or_create(&breeze_mock::data::for_blueprint(&bp.name), &bp.name).unwrap();
    let mut roots = rustls::RootCertStore::empty();
    for c in rustls::pki_types::CertificateDer::pem_slice_iter(ca.pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let config = Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let get = |name: &'static str| -> Result<u16, String> {
        let server_name = rustls::pki_types::ServerName::try_from(name).unwrap();
        let conn = rustls::ClientConnection::new(Arc::clone(&config), server_name).map_err(|e| e.to_string())?;
        let tcp = TcpStream::connect("127.0.0.1:29151").unwrap();
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        write!(tls, "GET /api/health HTTP/1.1\r\nHost: {name}\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
        let mut r = BufReader::new(tls);
        Ok(http::read_response_head(&mut r).map_err(|e| e.to_string())?.status)
    };
    assert_eq!(get("secure.test"), Ok(200));
    assert!(get("elsewhere.test").is_err(), "a certificate for another name must not verify");
}

#[test]
fn the_console_changes_what_breeze_core_reads() {
    let (_bp, _r) = start(
        "console",
        &format!(
            r#"{}
{}
  <console address="127.0.0.1" port="29169"/>"#,
            units("127.91.6"),
            server("s", "4.3.2", 29161, "")
        ),
    );
    let s = "127.0.0.1:29161";
    let c = "127.0.0.1:29169";
    let key = "s-key-not-a-secret-01";
    let world = call(c, "GET", "/api/world", &[], None).json();
    assert_eq!(world["units"].as_array().unwrap().len(), 2);
    assert_eq!(world["servers"][0]["name"], "s");

    assert_eq!(call(c, "POST", "/api/outdoor", &[], Some(json!({"celsius": 33}))).status, 200);
    let a = id_of(s, key, "Alpha");
    let state = call(s, "GET", &format!("/api/units/{a}/state"), &auth(key), None).json();
    assert_eq!(state["outdoor_temperature"], 33.0);

    assert_eq!(call(c, "POST", "/api/units/b/offline", &[], Some(json!({"offline": true}))).status, 200);
    let batch = call(s, "GET", "/api/units/state", &auth(key), None).json();
    let failed: Vec<&str> = batch["errors"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap()).collect();
    assert_eq!(failed, ["Bravo"]);

    assert_eq!(call(c, "POST", "/api/units/nobody/offline", &[], Some(json!({"offline": true}))).status, 404);
    assert_eq!(call(c, "POST", "/api/time-scale", &[], Some(json!({"scale": -1}))).status, 422);
    let page = String::from_utf8(call(c, "GET", "/", &[], None).body).unwrap();
    assert!(page.contains("<h1>breeze-mock: console</h1>") && page.contains("Bring back online"));
}

#[test]
fn an_exported_config_is_breeze_cores_own() {
    let bp = load("export", &format!("{}\n{}", units("127.91.7"), server("s", "4.3.2", 29171, "")));
    let path = std::env::temp_dir().join(format!("bm-export-{}.json", std::process::id()));
    breeze_mock::export::breeze_core_config(&bp.source, Some("s"), Some(&path)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.ends_with('\n'), "Breeze Core writes no trailing newline");
    let config: breeze_store::AppConfig = breeze_store::load(&path).unwrap();
    assert_eq!(config.api_key.as_deref(), Some("s-key-not-a-secret-01"));
    assert_eq!(config.units.len(), 2);
    let a = &config.units[0];
    assert_eq!((a.name.as_str(), a.ip.as_str(), a.port), ("Alpha", "127.91.7.2", 6444));
    assert_eq!(a.id as u64, bp.units[0].id);
    assert_eq!(a.token.as_ref().unwrap().len(), 128);
    assert_eq!(a.key.as_ref().unwrap().len(), 64);
}

#[test]
fn units_sharing_an_address_each_answer_a_hello() {
    let (bp, _r) = start(
        "shared",
        r#"<network><segment name="lo" cidr="127.91.8.0/24" mode="loopback"/></network>
  <unit key="one" segment="lo" ip="127.91.8.9"><epithets name="One"/><v3 port="6444" token="auto" key="auto"/></unit>
  <unit key="two" segment="lo" ip="127.91.8.9"><epithets name="Two"/><v3 port="6450" token="auto" key="auto"/></unit>"#,
    );
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    sock.send_to(&breeze_proto::discover::DISCOVERY_MSG, "127.91.8.9:6445").unwrap();
    let mut found = Vec::new();
    let mut buf = [0u8; 1024];
    while let Ok((n, from)) = sock.recv_from(&mut buf) {
        let SocketAddr::V4(v4) = from else { continue };
        let d = breeze_proto::discover::parse(*v4.ip(), &buf[..n]).unwrap();
        found.push((d.id, d.port));
        if found.len() == 2 {
            break;
        }
    }
    found.sort();
    let mut want: Vec<(u64, u16)> = bp.units.iter().map(|u| (u.id, u.protocol.port)).collect();
    want.sort();
    assert_eq!(found, want);
}

#[test]
fn an_address_this_machine_lacks_is_named_before_anything_starts() {
    let bp = load(
        "missing",
        r#"<network><segment name="lan" cidr="10.250.250.0/24" mode="existing"/></network>
  <unit key="u" segment="lan" ip="10.250.250.250"><epithets name="U"/><v3 token="auto" key="auto"/></unit>"#,
    );
    let e = run::start(&bp).err().expect("10.250.250.250 is not this machine's");
    assert!(e.contains("cannot use 10.250.250.250") && e.contains("already has"), "{e}");
}
