//! The console: a running world, inspected and poked over HTTP.
//!
//! `GET /` is a page for a person; everything else is JSON for scripts and
//! tests. The page's forms post to the same endpoints, so there is one set of
//! rules for both.
//!
//!   GET  /api/world                      rooms, units, servers, the weather
//!   POST /api/outdoor                    {"celsius": 31.5} or {"celsius": null}
//!   POST /api/time-scale                 {"scale": 60}
//!   POST /api/rooms/{id}/temperature     {"celsius": 26}
//!   POST /api/units/{key}/offline        {"offline": true}
//!   POST /api/units/{key}/error          {"code": 5}
//!   POST /api/servers/{name}/approve     {"code": "ABCD-EFGH"}
//!
//! No authentication: it listens where the blueprint says, which should be
//! loopback, and it can only change the simulation.

use std::io::{BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::api::front::MockServer;
use crate::api::http::{self, Request};
use crate::sim::Shared;
use crate::{debug, info};

pub struct Console {
    pub world: Shared,
    pub servers: Vec<Arc<MockServer>>,
}

pub fn serve(console: Console, addr: SocketAddr) -> Result<(), String> {
    let socket = TcpListener::bind(addr).map_err(|e| format!("console: cannot listen on {addr}: {e}"))?;
    info!("console", "http://{addr}/");
    let console = Arc::new(console);
    std::thread::spawn(move || {
        for stream in socket.incoming().flatten() {
            let console = Arc::clone(&console);
            std::thread::spawn(move || {
                if let Err(e) = connection(&console, stream) {
                    debug!("console", "{e}");
                }
            });
        }
    });
    Ok(())
}

fn connection(c: &Console, mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let Some(req) = http::read_request(&mut BufReader::new(&mut stream))? else {
        return Ok(());
    };
    let from_form = is_form(&req);
    match route(c, &req) {
        Ok(Reply::Page(html)) => http::respond(&mut stream, 200, "text/html; charset=utf-8", html.as_bytes()),
        Ok(Reply::Json(v)) if from_form => see_other(&mut stream, v),
        Ok(Reply::Json(v)) => http::respond(&mut stream, 200, "application/json", v.to_string().as_bytes()),
        Err((status, message)) if from_form => {
            let page = page(c, Some(&message));
            http::respond(&mut stream, status, "text/html; charset=utf-8", page.as_bytes())
        }
        Err((status, message)) => http::detail(&mut stream, status, &message),
    }
}

/// After a form, back to the page, so a reload does not post it again.
fn see_other(w: &mut impl Write, _: Value) -> std::io::Result<()> {
    w.write_all(b"HTTP/1.1 303 See Other\r\nLocation: /\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
    w.flush()
}

enum Reply {
    Page(String),
    Json(Value),
}

type Outcome = Result<Reply, (u16, String)>;

fn bad(message: impl Into<String>) -> (u16, String) {
    (422, message.into())
}

/// A form from the page, rather than a script. Judged by the body as well as
/// the header: `curl -d '{...}'` labels JSON as a form unless told otherwise,
/// and a script that forgets would otherwise be answered with a web page.
fn is_form(req: &Request) -> bool {
    req.header("content-type")
        .is_some_and(|t| t.starts_with("application/x-www-form-urlencoded"))
        && !serde_json::from_slice::<Value>(&req.body).is_ok_and(|v| v.is_object())
}

/// The request's fields, from JSON or from a form.
fn fields(req: &Request) -> Result<Value, (u16, String)> {
    if is_form(req) {
        let mut obj = serde_json::Map::new();
        for pair in String::from_utf8_lossy(&req.body).split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = decode(v);
            // A form sends text; read numbers and the two words as what they mean.
            let value = match v.as_str() {
                "" | "null" => Value::Null,
                "true" | "on" => Value::Bool(true),
                "false" | "off" => Value::Bool(false),
                n => n.parse::<f64>().map(Value::from).unwrap_or(Value::String(v.clone())),
            };
            obj.insert(decode(k), value);
        }
        return Ok(Value::Object(obj));
    }
    serde_json::from_slice(&req.body).map_err(|e| bad(format!("the body is not JSON: {e}")))
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn number(f: &Value, key: &str) -> Result<f64, (u16, String)> {
    f.get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| bad(format!("{key} must be a number")))
}

fn route(c: &Console, req: &Request) -> Outcome {
    let path = req.path().trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match (req.method.as_str(), parts.as_slice()) {
        ("GET", []) => Ok(Reply::Page(page(c, None))),
        ("GET", ["api", "world"]) => Ok(Reply::Json(world(c))),
        ("POST", ["api", "outdoor"]) => {
            let f = fields(req)?;
            let celsius = match f.get("celsius") {
                None | Some(Value::Null) => None,
                Some(v) => Some(v.as_f64().ok_or_else(|| bad("celsius must be a number or null"))?),
            };
            if celsius.is_some_and(|t| !(-50.0..=60.0).contains(&t)) {
                return Err(bad("an outdoor temperature between -50 and 60, please"));
            }
            let mut w = lock(c)?;
            w.outdoor_override = celsius;
            info!("console", "outdoor temperature {}", celsius.map_or("follows the weather again".into(), |t| format!("held at {t}")));
            Ok(Reply::Json(json!({ "outdoor": w.outdoor(), "held": celsius.is_some() })))
        }
        ("POST", ["api", "time-scale"]) => {
            let scale = number(&fields(req)?, "scale")?;
            if !(0.0..=3600.0).contains(&scale) {
                return Err(bad("a time scale from 0 (paused) to 3600"));
            }
            lock(c)?.time_scale = scale;
            info!("console", "the simulation now runs at {scale}x");
            Ok(Reply::Json(json!({ "scale": scale })))
        }
        ("POST", ["api", "rooms", id, "temperature"]) => {
            let t = number(&fields(req)?, "celsius")?;
            if !(-30.0..=60.0).contains(&t) {
                return Err(bad("a room between -30 and 60, please"));
            }
            let mut w = lock(c)?;
            let room = w
                .rooms
                .iter_mut()
                .find(|r| r.spec.id == *id)
                .ok_or((404, format!("no room {id:?}")))?;
            room.temperature = t;
            info!("console", "room {id} set to {t}");
            Ok(Reply::Json(json!({ "room": id, "celsius": t })))
        }
        ("POST", ["api", "units", key, "offline"]) => {
            let f = fields(req)?;
            let offline = f.get("offline").and_then(Value::as_bool).ok_or_else(|| bad("offline must be true or false"))?;
            let mut w = lock(c)?;
            let i = w.by_key(key).ok_or((404, format!("no unit {key:?}")))?;
            w.units[i].forced_offline = offline;
            info!("console", "unit {key} {}", if offline { "taken offline" } else { "back online" });
            Ok(Reply::Json(json!({ "unit": key, "offline": offline })))
        }
        ("POST", ["api", "units", key, "error"]) => {
            let code = number(&fields(req)?, "code")?;
            if !(0.0..=255.0).contains(&code) || code.fract() != 0.0 {
                return Err(bad("an error code from 0 (none) to 255"));
            }
            let mut w = lock(c)?;
            let i = w.by_key(key).ok_or((404, format!("no unit {key:?}")))?;
            w.units[i].live.error_code = code as u8;
            info!("console", "unit {key} shows error code {code}");
            Ok(Reply::Json(json!({ "unit": key, "error_code": code as u8 })))
        }
        ("POST", ["api", "servers", name, "approve"]) => {
            let f = fields(req)?;
            let code = f.get("code").and_then(Value::as_str).ok_or_else(|| bad("code is the pairing code to approve"))?;
            let server = c
                .servers
                .iter()
                .find(|s| s.def.name == *name)
                .ok_or((404, format!("no server {name:?}")))?;
            server.approve(code).map_err(|e| (409, e))?;
            info!("console", "approved pairing code {code} on {name}");
            Ok(Reply::Json(json!({ "server": name, "approved": code })))
        }
        _ => Err((404, "Not Found".into())),
    }
}

fn lock(c: &Console) -> Result<std::sync::MutexGuard<'_, crate::sim::World>, (u16, String)> {
    c.world.lock().map_err(|_| (500, "the world is poisoned".to_string()))
}

fn mode_name(m: breeze_proto::ac::types::Mode) -> String {
    format!("{m:?}").to_ascii_uppercase().replace("FANONLY", "FAN_ONLY")
}

fn world(c: &Console) -> Value {
    let Ok(w) = c.world.lock() else { return json!({}) };
    let hour = w.hour();
    let units: Vec<Value> = w
        .units
        .iter()
        .map(|u| {
            let l = &u.live;
            json!({
                "key": u.def.key,
                "name": u.def.identity.name,
                "nickname": u.def.identity.nickname,
                "labels": u.def.identity.labels,
                "id": u.def.id.to_string(),
                "address": format!("{}:{}", u.def.ip, u.def.protocol.port),
                "protocol": if u.def.protocol.version == crate::blueprint::ProtocolVersion::V3 { "V3" } else { "V2" },
                "room": w.rooms[u.room].spec.id,
                "online": !u.offline_at(hour),
                "forced_offline": u.forced_offline,
                "power": l.power,
                "mode": mode_name(l.mode),
                "target": l.target,
                "fan": l.fan,
                "swing": format!("{:?}", l.swing).to_ascii_uppercase(),
                "eco": l.eco,
                "turbo": l.turbo,
                "error_code": l.error_code,
                "output_w": (u.output_w * 10.0).round() / 10.0,
                "last_refused": u.last_refused,
                "stats": u.stats,
            })
        })
        .collect();
    let rooms: Vec<Value> = w
        .rooms
        .iter()
        .map(|r| json!({ "id": r.spec.id, "name": r.spec.name, "celsius": (r.temperature * 100.0).round() / 100.0 }))
        .collect();
    let servers: Vec<Value> = c
        .servers
        .iter()
        .map(|s| {
            json!({
                "name": s.def.name,
                "release": s.def.build.version,
                "profile": s.def.build.profile,
                "listens": s.def.listens.iter().map(|l| format!("{}://{}:{}", if l.tls { "https" } else { "http" }, l.address, l.port)).collect::<Vec<_>>(),
                "domains": s.def.domains.iter().map(|d| d.name.clone()).collect::<Vec<_>>(),
                "breeze_core": s.backend.addr.to_string(),
                "store": s.backend.dir.display().to_string(),
                "approval": format!("{:?}", s.def.pairing.approve).to_ascii_lowercase(),
                "pending_codes": s.pending_codes().into_iter().map(|(code, label)| json!({ "code": code, "label": label })).collect::<Vec<_>>(),
                "stats": s.stats.lock().map(|st| st.clone()).unwrap_or_default(),
            })
        })
        .collect();
    json!({
        "blueprint": w.name,
        "time_of_day": format!("{:02}:{:02}", hour as u32, ((hour.fract()) * 60.0) as u32),
        "time_scale": w.time_scale,
        "outdoor": (w.outdoor() * 10.0).round() / 10.0,
        "outdoor_held": w.outdoor_override.is_some(),
        "rooms": rooms,
        "units": units,
        "servers": servers,
    })
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The page: what is happening, and a form for each thing that can be changed.
fn page(c: &Console, problem: Option<&str>) -> String {
    let v = world(c);
    let mut h = String::new();
    h.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    h.push_str(&format!("<title>breeze-mock: {}</title>", esc(v["blueprint"].as_str().unwrap_or(""))));
    h.push_str("<style>body{font:15px/1.5 system-ui,sans-serif;margin:16px;max-width:70em;color:#1a1a1a;background:#fafafa}\
        table{border-collapse:collapse;margin:8px 0 20px}th,td{border:1px solid #ccc;padding:4px 8px;text-align:left;vertical-align:top}\
        th{background:#eee}form{display:inline-flex;gap:4px;align-items:center;margin:2px 0}input{width:6em}\
        .problem{background:#fde8e8;border:1px solid #c33;padding:8px}@media (prefers-color-scheme:dark){body{background:#151515;color:#eee}th{background:#2a2a2a}th,td{border-color:#444}}\
        </style></head><body><main>");
    h.push_str(&format!("<h1>breeze-mock: {}</h1>", esc(v["blueprint"].as_str().unwrap_or(""))));
    if let Some(p) = problem {
        h.push_str(&format!("<p class=\"problem\" role=\"alert\">{}</p>", esc(p)));
    }
    h.push_str(&format!(
        "<p>Simulated time {} at {}x. Outdoors {} °C{}. <a href=\"/\">Refresh</a></p>",
        v["time_of_day"].as_str().unwrap_or(""),
        v["time_scale"],
        v["outdoor"],
        if v["outdoor_held"].as_bool() == Some(true) { " (held)" } else { "" }
    ));
    h.push_str("<form method=\"post\" action=\"/api/outdoor\"><label for=\"out\">Hold outdoors at</label>\
        <input id=\"out\" name=\"celsius\" inputmode=\"decimal\" placeholder=\"°C\"><button>Hold</button></form> \
        <form method=\"post\" action=\"/api/outdoor\"><input type=\"hidden\" name=\"celsius\" value=\"null\"><button>Follow the weather</button></form> \
        <form method=\"post\" action=\"/api/time-scale\"><label for=\"ts\">Time scale</label>\
        <input id=\"ts\" name=\"scale\" inputmode=\"decimal\" placeholder=\"1\"><button>Set</button></form>");

    h.push_str("<h2>Units</h2><table><thead><tr><th scope=\"col\">Unit</th><th scope=\"col\">Address</th><th scope=\"col\">Set to</th>\
        <th scope=\"col\">Room</th><th scope=\"col\">Requests</th><th scope=\"col\">Change</th></tr></thead><tbody>");
    let rooms = v["rooms"].as_array().cloned().unwrap_or_default();
    for u in v["units"].as_array().into_iter().flatten() {
        let key = esc(u["key"].as_str().unwrap_or(""));
        let room = rooms.iter().find(|r| r["id"] == u["room"]);
        let st = &u["stats"];
        h.push_str(&format!(
            "<tr><th scope=\"row\">{} <small>({key})</small>{}</th><td>{} {}</td><td>{}</td><td>{} {} °C</td><td>{} queries, {} controls, {} dropped</td><td>\
             <form method=\"post\" action=\"/api/units/{key}/offline\"><input type=\"hidden\" name=\"offline\" value=\"{}\"><button>{}</button></form> \
             <form method=\"post\" action=\"/api/units/{key}/error\"><label for=\"e-{key}\">Error code</label><input id=\"e-{key}\" name=\"code\" inputmode=\"numeric\" value=\"{}\"><button>Show</button></form></td></tr>",
            esc(u["name"].as_str().unwrap_or("")),
            if u["online"].as_bool() == Some(false) { " — <strong>offline</strong>" } else { "" },
            esc(u["address"].as_str().unwrap_or("")),
            u["protocol"].as_str().unwrap_or(""),
            if u["power"].as_bool() == Some(true) {
                format!("{} {} °C, fan {}, swing {}{}{}", u["mode"].as_str().unwrap_or("").to_lowercase(), u["target"], u["fan"], u["swing"].as_str().unwrap_or("").to_lowercase(),
                    if u["eco"].as_bool() == Some(true) { ", eco" } else { "" }, if u["turbo"].as_bool() == Some(true) { ", turbo" } else { "" })
            } else {
                format!("off (set to {} {} °C)", u["mode"].as_str().unwrap_or("").to_lowercase(), u["target"])
            },
            room.map(|r| esc(r["name"].as_str().unwrap_or(""))).unwrap_or_default(),
            room.map(|r| r["celsius"].to_string()).unwrap_or_default(),
            st["queries"], st["controls"], st["dropped"],
            if u["forced_offline"].as_bool() == Some(true) { "false" } else { "true" },
            if u["forced_offline"].as_bool() == Some(true) { "Bring back online" } else { "Take offline" },
            u["error_code"],
        ));
    }
    h.push_str("</tbody></table><h2>Rooms</h2><table><thead><tr><th scope=\"col\">Room</th><th scope=\"col\">Now</th><th scope=\"col\">Change</th></tr></thead><tbody>");
    for r in &rooms {
        let id = esc(r["id"].as_str().unwrap_or(""));
        h.push_str(&format!(
            "<tr><th scope=\"row\">{}</th><td>{} °C</td><td><form method=\"post\" action=\"/api/rooms/{id}/temperature\">\
             <label for=\"r-{id}\">Set to</label><input id=\"r-{id}\" name=\"celsius\" inputmode=\"decimal\"><button>Set</button></form></td></tr>",
            esc(r["name"].as_str().unwrap_or("")),
            r["celsius"]
        ));
    }
    h.push_str("</tbody></table><h2>Servers</h2>");
    for s in v["servers"].as_array().into_iter().flatten() {
        let name = esc(s["name"].as_str().unwrap_or(""));
        h.push_str(&format!(
            "<h3>{name}</h3><p>Breeze Core {} ({} profile) on {}{}. Pairing: {}. {} requests, {} faults injected.</p>",
            esc(s["release"].as_str().unwrap_or("")),
            esc(s["profile"].as_str().unwrap_or("")),
            esc(&s["listens"].as_array().into_iter().flatten().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")),
            {
                let d: Vec<&str> = s["domains"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                if d.is_empty() { String::new() } else { format!(" as {}", esc(&d.join(", "))) }
            },
            esc(s["approval"].as_str().unwrap_or("")),
            s["stats"]["requests"],
            s["stats"]["faults"],
        ));
        let pending = s["pending_codes"].as_array().cloned().unwrap_or_default();
        if pending.is_empty() {
            h.push_str("<p>No pairing codes are waiting.</p>");
        }
        for p in pending {
            let code = esc(p["code"].as_str().unwrap_or(""));
            h.push_str(&format!(
                "<form method=\"post\" action=\"/api/servers/{name}/approve\"><input type=\"hidden\" name=\"code\" value=\"{code}\">\
                 <button>Approve {code}{}</button></form><br>",
                p["label"].as_str().filter(|l| !l.is_empty()).map(|l| format!(" for {}", esc(l))).unwrap_or_default()
            ));
        }
    }
    h.push_str("</main></body></html>");
    h
}
