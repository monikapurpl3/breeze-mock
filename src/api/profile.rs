//! Making the Breeze Core behind a mock server look like the release its
//! `<build>` names.
//!
//! The backend is always the newest Breeze Core. An older profile is that,
//! with what the older release lacked taken away: its routes answer 404 as
//! they did before they existed, the fields it never sent are removed, and
//! `/api/version` and `/api/system` say what it said. That is the part of an
//! older release a client can observe and branch on -- which is exactly what
//! the feature list exists for.

use serde_json::Value;

use super::http::Request;
use crate::blueprint::{Build, Host, Server};

/// The feature a route belongs to, if it can be absent.
fn feature_of(method: &str, path: &str) -> Option<&'static str> {
    let units_tail = path.strip_prefix("/api/units/").and_then(|r| r.split_once('/'));
    Some(match (method, path) {
        (_, "/api/units/state") => "batch_state",
        (_, "/api/units/stream") => "live_stream",
        (_, "/api/units/scan") => "unit_scan",
        (_, "/api/config") => "config_api",
        (_, "/api/system") => "system_info",
        (_, "/metrics") => "metrics",
        (_, "/api/auth/whoami") => "whoami",
        (_, "/api/auth/upgrade") => "ed25519_auth",
        (_, p) if p.starts_with("/api/auth/enroll/") || p == "/api/auth/devices" || p.starts_with("/api/auth/devices/") => {
            "device_pairing"
        }
        (_, p) if p == "/api/timers" || p.starts_with("/api/timers/") => "sleep_timer",
        (_, p) if p == "/api/programs" || p.starts_with("/api/programs/") => "programs",
        ("DELETE", p) if p.starts_with("/api/units/") && units_tail.is_none() => "delete_unit",
        _ => match units_tail {
            Some((_, "capabilities")) => "unit_capabilities",
            Some((_, "history")) => "unit_history",
            _ => return None,
        },
    })
}

/// Whether this build has the route at all.
pub fn has_route(build: &Build, method: &str, path: &str) -> bool {
    feature_of(method, path).is_none_or(|f| build.has(f))
}

/// What the request becomes before Breeze Core sees it, or the refusal an
/// older release would have given instead.
pub fn rewrite_request(build: &Build, req: &mut Request) -> Result<(), (u16, String)> {
    if !build.has("compression") {
        req.remove_header("accept-encoding");
    }
    let path = req.path().to_string();
    let control = path.starts_with("/api/units/") && path.ends_with("/control") && req.method == "POST";
    let timer = path == "/api/timers" && req.method == "POST";
    let enroll = path == "/api/auth/enroll/start" && req.method == "POST";
    if !(control || timer || enroll) {
        return Ok(());
    }
    let Ok(mut body) = serde_json::from_slice::<Value>(&req.body) else {
        return Ok(()); // Breeze Core gives the parse error itself
    };
    let Some(obj) = body.as_object_mut() else {
        return Ok(());
    };
    if control && !build.has("beep_control") {
        // Releases before beep_control ignored the field.
        obj.remove("beep");
    }
    if timer && !build.has("timer_at") {
        // A release without timer_at did not know these fields. Without them
        // Breeze Core itself refuses a timer that has no minutes -- after
        // authenticating the request, in the order a real server does it.
        obj.remove("at");
        obj.remove("days");
    }
    if enroll {
        let wants = obj.get("auth_version").and_then(Value::as_u64).unwrap_or(1) as u8;
        if !build.auth_versions.contains(&wants) {
            return Err((400, format!("auth_version {wants} is not supported by this server")));
        }
    }
    req.body = serde_json::to_vec(&body).unwrap_or_default();
    Ok(())
}

/// Whether the response to this request needs reading whole to be changed.
pub fn rewrites_response(build: &Build, req: &Request) -> bool {
    let p = req.path();
    match (req.method.as_str(), p) {
        ("GET", "/api/version") | ("GET", "/api/system") => true,
        ("POST", _) if p.starts_with("/api/units/") && p.ends_with("/control") => !build.has("control_feedback"),
        (_, p) if p == "/api/timers" || p.starts_with("/api/timers/") => !build.has("timer_at"),
        _ => false,
    }
}

fn set(v: &mut Value, key: &str, new: Value) {
    if let Some(obj) = v.as_object_mut() {
        if obj.contains_key(key) {
            obj.insert(key.to_string(), new);
        }
    }
}

pub fn rewrite_response(server: &Server, listen: &str, tls: bool, req: &Request, body: &mut Value) {
    let b = &server.build;
    match req.path() {
        "/api/version" => {
            set(body, "version", b.version.clone().into());
            set(body, "commit", b.commit.clone().into());
            set(body, "features", b.features.clone().into());
            set(body, "auth_versions", b.auth_versions.clone().into());
        }
        "/api/system" => system(server, listen, tls, body),
        p if p == "/api/timers" || p.starts_with("/api/timers/") => strip_kind(body),
        _ => {
            if let Some(obj) = body.as_object_mut() {
                obj.remove("not_applied");
            }
        }
    }
}

fn strip_kind(v: &mut Value) {
    match v {
        Value::Array(items) => items.iter_mut().for_each(strip_kind),
        Value::Object(obj) => {
            obj.remove("kind");
            if let Some(list) = obj.get_mut("timers") {
                strip_kind(list);
            }
        }
        _ => {}
    }
}

/// The machine the blueprint says this server runs on, in place of the one
/// breeze-mock really runs on -- which a mock should neither claim to be nor
/// leak the paths of.
fn system(server: &Server, listen: &str, tls: bool, v: &mut Value) {
    let b = &server.build;
    let h: &Host = &server.host;
    if let Some(s) = v.get_mut("server") {
        set(s, "version", b.version.clone().into());
        set(s, "commit", b.commit.clone().into());
        set(s, "features", b.features.clone().into());
        set(s, "auth_versions", b.auth_versions.clone().into());
        set(s, "timezone", h.timezone.clone().into());
        set(s, "utc_offset_seconds", h.utc_offset_seconds.into());
    }
    if let Some(os) = v.get_mut("os") {
        let (id, version) = distro(&h.os);
        set(os, "system", "Linux".into());
        set(os, "hostname", h.hostname.clone().into());
        set(os, "pretty_name", h.os.clone().into());
        set(os, "distribution", h.os.clone().into());
        set(os, "distro_id", id.clone());
        set(os, "distribution_id", id);
        set(os, "distro_version", version);
        set(os, "kernel", h.kernel.clone().into());
        set(os, "kernel_version", "#1 SMP PREEMPT_DYNAMIC".into());
        set(os, "platform", "linux".into());
        set(os, "family", "unix".into());
        set(os, "arch", h.arch.clone().into());
        // What a packaged Linux build reports: one static musl executable.
        set(os, "libc", "musl (static)".into());
    }
    if let Some(init) = v.get_mut("init") {
        set(init, "name", h.init.clone().into());
        set(init, "detail", init_detail(&h.init).into());
    }
    // A Linux machine's uptime, which the host breeze-mock runs on may not
    // have (Windows reports none): the machine came up with the server.
    if let Some(up) = v.get("server").and_then(|s| s.get("uptime_seconds")).cloned() {
        set(v, "machine_uptime_seconds", up);
    }
    // Plain HTTP is Breeze Core reached directly. TLS is Breeze Core behind a
    // proxy that terminates it, which is the only way Breeze Core serves TLS,
    // and that is what the backend already reports.
    if !tls {
        if let Some(c) = v.get_mut("connection") {
            set(c, "forwarded_for", Value::Null);
            set(c, "behind_proxy_enabled", false.into());
        }
        if let Some(s) = v.get_mut("settings") {
            set(s, "behind_proxy", false.into());
        }
    }
    if let Some(cpu) = v.get_mut("cpu") {
        set(cpu, "model", h.cpu.clone().into());
        set(cpu, "cores", h.cpus.into());
        set(cpu, "arch", h.arch.clone().into());
        set(cpu, "available_parallelism", h.cpus.into());
    }
    if let Some(net) = v.get_mut("network") {
        let (host, port) = listen.rsplit_once(':').unwrap_or((listen, ""));
        set(net, "hostname", h.hostname.clone().into());
        set(net, "local_addresses", vec![host.to_string()].into());
        set(net, "bind", listen.into());
        set(net, "bind_host", host.into());
        set(net, "bind_port", port.into());
    }
    if let Some(paths) = v.get_mut("paths") {
        for (key, file) in [
            ("config", "config.json"),
            ("devices", "devices.json"),
            ("programs", "programs.json"),
            ("timers", "timers.json"),
        ] {
            set(paths, key, format!("/etc/breeze-core/{file}").into());
        }
        set(paths, "package", "/usr/bin/breeze-core".into());
    }
    if let Some(storage) = v.get_mut("storage") {
        for (key, file) in [
            ("config", "config.json"),
            ("devices", "devices.json"),
            ("programs", "programs.json"),
            ("timers", "timers.json"),
        ] {
            if let Some(f) = storage.get_mut(key) {
                set(f, "path", format!("/etc/breeze-core/{file}").into());
            }
        }
    }
    if let Some(p) = v.get_mut("process") {
        if let Some(obj) = p.as_object_mut() {
            obj.remove("rss_bytes");
        }
    }
}

/// What os-release would say for a PRETTY_NAME: `AlmaLinux 9.6 (Sage Margay)`
/// is `almalinux`, `9.6`.
fn distro(pretty: &str) -> (Value, Value) {
    let id: String = pretty
        .split_whitespace()
        .next()
        .unwrap_or("")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    let version = pretty
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        .map(|w| w.trim_end_matches(|c: char| !c.is_ascii_alphanumeric()).to_string());
    (
        if id.is_empty() { Value::Null } else { id.into() },
        version.map(Value::from).unwrap_or(Value::Null),
    )
}

/// The detail Breeze Core gives with each init system it recognises.
fn init_detail(init: &str) -> &'static str {
    match init {
        "systemd" => "systemctl status breeze-core",
        "openrc" => "rc-service breeze-core status",
        "procd" => "/etc/init.d/breeze-core status",
        "runit" | "termux-services" => "sv status breeze-core",
        "s6" => "s6-rc -a list",
        "rc.d" => "service breeze_core status",
        "launchd" => "launchctl print system/breeze-core",
        "windows-sc" => "Windows Service Control Manager",
        _ => "started from a shell",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn routes_belong_to_their_features() {
        assert_eq!(feature_of("GET", "/api/units/state"), Some("batch_state"));
        assert_eq!(feature_of("GET", "/api/units/123/capabilities"), Some("unit_capabilities"));
        assert_eq!(feature_of("GET", "/api/units/123/history"), Some("unit_history"));
        assert_eq!(feature_of("DELETE", "/api/units/123"), Some("delete_unit"));
        assert_eq!(feature_of("PATCH", "/api/units/123"), None, "rename is in every release");
        assert_eq!(feature_of("POST", "/api/units/123/control"), None);
        assert_eq!(feature_of("DELETE", "/api/timers/abc"), Some("sleep_timer"));
        assert_eq!(feature_of("POST", "/api/auth/enroll/start"), Some("device_pairing"));
        assert_eq!(feature_of("GET", "/api/health"), None);
    }

    #[test]
    fn a_pretty_name_gives_os_release_fields() {
        let d = |s| distro(s);
        assert_eq!(d("AlmaLinux 9.6 (Sage Margay)"), (json!("almalinux"), json!("9.6")));
        assert_eq!(d("Debian GNU/Linux 12 (bookworm)"), (json!("debian"), json!("12")));
        assert_eq!(d("Ubuntu 24.04.1 LTS"), (json!("ubuntu"), json!("24.04.1")));
        assert_eq!(d("Arch Linux"), (json!("arch"), Value::Null));
    }
}
