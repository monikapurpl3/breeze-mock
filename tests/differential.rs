//! A mock server against the real thing: the same store, the same emulated
//! units, the same requests -- and the same answers, byte for byte where they
//! can be, and key for key where a reading moves between the two calls.
//!
//! Needs a Breeze Core binary of the release the profile names:
//!
//!   BREEZE_CORE_BIN=/path/to/breeze-core cargo test --test differential
//!
//! Skipped without it. This is the check that the front adds nothing a client
//! could tell apart from Breeze Core itself, apart from what it is asked to.

use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use breeze_mock::api::http;
use serde_json::{json, Value};

const TOKEN: &str = "diff-device-token-not-a-secret-0123456789";
const KEY: &str = "diff-key-not-a-secret-0123";

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn call(addr: &str, method: &str, path: &str, auth: bool, body: Option<Value>) -> Answer {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if auth {
        req.push_str(&format!("X-API-Key: {KEY}\r\nAuthorization: Bearer {TOKEN}\r\n"));
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
    Answer {
        status: head.status,
        headers: head.headers,
        body,
    }
}

/// The headers a client could act on: everything but framing and dates.
fn headers(a: &Answer) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = a
        .headers
        .iter()
        .map(|(n, v)| (n.to_ascii_lowercase(), v.clone()))
        .filter(|(n, _)| !matches!(n.as_str(), "connection" | "date" | "content-length" | "transfer-encoding" | "server"))
        .collect();
    h.sort();
    h
}

/// Keys and value types, recursively, for answers whose readings move.
fn shape(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(shape).collect()),
        Value::Number(_) => json!("number"),
        Value::String(_) => json!("string"),
        Value::Bool(_) => json!("bool"),
        Value::Null => json!("null"),
    }
}

#[test]
fn a_mock_server_answers_as_breeze_core_does() {
    let Some(bin) = std::env::var_os("BREEZE_CORE_BIN") else {
        eprintln!("skipped: set BREEZE_CORE_BIN to a breeze-core binary to compare against");
        return;
    };
    let data = std::env::temp_dir().join(format!("bm-diff-data-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    std::env::set_var("BREEZE_MOCK_DATA", &data);
    breeze_mock::log::set_level(0);

    let path = data.join("diff.xml");
    std::fs::write(
        &path,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<blueprint xmlns="urn:breeze-mock:blueprint:1" version="1" name="diff" seed="9">
  <environment><constant outdoor="10"/></environment>
  <network><segment name="lo" cidr="127.92.1.0/24" mode="loopback" discovery="false"/></network>
  <unit key="a" segment="lo" ip="127.92.1.2">
    <epithets name="Alpha"/><v3 token="auto" key="auto"/>
    <state power="on" mode="cool" target="24"/><behaviour reply-delay="0ms"/>
  </unit>
  <unit key="b" segment="lo" ip="127.92.1.3">
    <epithets name="Bravo"/><v3 token="auto" key="auto"/>
    <capabilities modes="auto cool heat fan-only" swing="vertical" fan="low high auto" eco="false" turbo="cool"/>
    <state power="on" mode="heat" target="22"/><behaviour reply-delay="0ms"/>
  </unit>
  <server name="mock" api-key="{KEY}">
    <build profile="4.3.2"/>
    <listen address="127.0.0.1" port="29201"/>
    <pairing approve="manual"><device label="diff" token="{TOKEN}"/></pairing>
    <manage unit="a"/><manage unit="b"/>
  </server>
</blueprint>
"#
        ),
    )
    .unwrap();
    let bp = breeze_mock::blueprint::load(&path).unwrap();
    let running = breeze_mock::run::start(&bp).unwrap();

    // The real one, on a copy of the mock's own store: same key, same device.
    let real_dir = data.join("real");
    std::fs::create_dir_all(&real_dir).unwrap();
    for f in ["config.json", "devices.json"] {
        std::fs::copy(running.servers[0].backend.dir.join(f), real_dir.join(f)).unwrap();
    }
    let _real = Kill(
        Command::new(bin)
            .arg("serve")
            .env("AC_CONFIG_DIR", &real_dir)
            .env("BREEZE_HOST", "127.0.0.1")
            .env("BREEZE_PORT", "29202")
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while TcpStream::connect("127.0.0.1:29202").is_err() {
        assert!(Instant::now() < deadline, "the real breeze-core did not start");
        std::thread::sleep(Duration::from_millis(50));
    }

    let mock = "127.0.0.1:29201";
    let real = "127.0.0.1:29202";
    let id = {
        let units: Value = serde_json::from_slice(&call(real, "GET", "/api/units", true, None).body).unwrap();
        units[1]["id"].as_str().unwrap().to_string()
    };

    // Identical, byte for byte.
    let exact: Vec<(&str, String, bool, Option<Value>)> = vec![
        ("GET", "/api/health".into(), false, None),
        ("GET", "/api/version".into(), true, None),
        ("GET", "/api/units".into(), true, None),
        ("GET", format!("/api/units/{id}/capabilities"), true, None),
        ("GET", "/api/units/999/state".into(), true, None),
        ("GET", "/api/units".into(), false, None),
        ("GET", "/api/nope".into(), true, None),
        ("PUT", "/api/units".into(), true, None),
        ("POST", format!("/api/units/{id}/control"), true, Some(json!({"target_temperature": 99}))),
        ("POST", format!("/api/units/{id}/control"), true, Some(json!({"operational_mode": "WARP"}))),
        ("POST", "/api/timers".into(), true, Some(json!({"unit_ids": [id.clone()]}))),
        ("GET", "/api/programs".into(), true, None),
        ("GET", "/api/auth/whoami".into(), false, None),
    ];
    for (method, path, auth, body) in exact {
        let m = call(mock, method, &path, auth, body.clone());
        let r = call(real, method, &path, auth, body);
        assert_eq!(m.status, r.status, "{method} {path}: status");
        // Not vacuous: Breeze Core sends its hardening headers on every answer.
        assert!(
            headers(&r).iter().any(|(n, _)| n == "x-content-type-options"),
            "{method} {path}: no security headers to compare: {:?}",
            headers(&r)
        );
        assert_eq!(headers(&m), headers(&r), "{method} {path}: headers");
        assert_eq!(
            String::from_utf8_lossy(&m.body),
            String::from_utf8_lossy(&r.body),
            "{method} {path}: body"
        );
    }

    // And the comparison can fail: a different profile on the same store
    // must not pass for the real thing.
    let v: Value = serde_json::from_slice(&call(real, "GET", "/api/version", true, None).body).unwrap();
    assert_eq!((v["version"].as_str(), v["units"].as_u64()), (Some("4.3.2"), Some(2)));
    assert_ne!(shape(&v), shape(&json!({})));

    // Readings move between two calls; the shape may not.
    for path in [format!("/api/units/{id}/state"), "/api/units/state".to_string()] {
        let m = call(mock, "GET", &path, true, None);
        let r = call(real, "GET", &path, true, None);
        assert_eq!(m.status, r.status, "{path}: status");
        let (m, r): (Value, Value) = (serde_json::from_slice(&m.body).unwrap(), serde_json::from_slice(&r.body).unwrap());
        assert_eq!(shape(&m), shape(&r), "{path}: shape");
    }
    // A control the unit partly refuses, through each.
    let m = call(mock, "POST", &format!("/api/units/{id}/control"), true, Some(json!({"turbo": true})));
    let r = call(real, "POST", &format!("/api/units/{id}/control"), true, Some(json!({"turbo": true})));
    let (m, r): (Value, Value) = (serde_json::from_slice(&m.body).unwrap(), serde_json::from_slice(&r.body).unwrap());
    assert_eq!(m["not_applied"], r["not_applied"]);
    assert_eq!(shape(&m), shape(&r));
    drop(running);
}
