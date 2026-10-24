//! The real Breeze Core behind each mock server.
//!
//! Breeze Core's HTTP layer, unmodified, serving on a loopback port of its
//! own and driving the emulated units over the real LAN protocol. Everything a
//! client sees beyond what the front changes -- pairing, signed requests,
//! timers, the scheduler, the event stream, `not_applied`, every status code
//! -- is Breeze Core's own, because it is Breeze Core's own code.

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use breeze_http::{AppState, Settings};
use breeze_store::{AppConfig, DeviceRecord, DevicesDoc, Mode, UnitConfig};
use sha2::Digest as _;

use crate::blueprint::{Blueprint, ProtocolVersion, Server, Unit};

pub struct Backend {
    pub addr: SocketAddr,
    pub state: Arc<AppState>,
    pub dir: PathBuf,
    /// Removed at the end of the run: the server had no `state-dir=`.
    temporary: bool,
}

impl Drop for Backend {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One unit as Breeze Core's `config.json` records it.
pub fn unit_config(u: &Unit) -> UnitConfig {
    let v3 = u.protocol.version == ProtocolVersion::V3;
    UnitConfig {
        name: u.identity.name.clone(),
        ip: u.ip.to_string(),
        port: u.protocol.port,
        id: u.id as i64,
        token: v3.then(|| hex(&u.protocol.token)),
        key: v3.then(|| hex(&u.protocol.key)),
    }
}

/// A `config.json` for these units: what a mock server is given, and what
/// `breeze-mock export breeze-core-config` writes for a real one.
pub fn app_config<'a>(units: impl IntoIterator<Item = &'a Unit>, api_key: &str) -> AppConfig {
    AppConfig {
        api_key: Some(api_key.to_string()),
        units: units.into_iter().map(unit_config).collect(),
    }
}

fn managed<'a>(bp: &'a Blueprint, server: &'a Server) -> impl Iterator<Item = &'a Unit> {
    server
        .units
        .iter()
        .filter_map(|k| bp.units.iter().find(|u| &u.key == k))
}

fn now() -> f64 {
    breeze_auth::signing::now_seconds()
}

/// Put the blueprint's preset devices into `devices.json`, keeping any a
/// client paired in an earlier run.
fn preset_devices(bp: &Blueprint, server: &Server, path: &Path) -> Result<(), String> {
    let mut doc: DevicesDoc = breeze_store::load(path).map_err(|e| e.to_string())?;
    for d in &server.pairing.devices {
        let hash = breeze_auth::verify::hash_secret(&d.token);
        if doc.devices.iter().any(|r| r.token_hash.as_deref() == Some(&hash)) {
            continue;
        }
        let id = sha2::Sha256::digest(format!("{}/{}/{}", bp.seed, server.name, d.label));
        doc.devices.push(DeviceRecord {
            token_id: hex(&id[..8]),
            label: d.label.clone(),
            auth_version: 1,
            token_hash: Some(hash),
            public_key: None,
            created_at: now(),
            expires_at: None,
            last_used: None,
        });
    }
    breeze_store::save(path, &doc, Mode::Private).map_err(|e| e.to_string())
}

/// A free loopback port. Released before Breeze Core binds it; nothing else
/// on a test machine races for it in that instant.
fn free_port() -> Result<u16, String> {
    let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
    Ok(l.local_addr().map_err(|e| e.to_string())?.port())
}

pub fn start(bp: &Blueprint, server: &Server, run_dir: &Path) -> Result<Backend, String> {
    let (dir, temporary) = match &server.state_dir {
        Some(d) => (d.clone(), false),
        None => (run_dir.join(&server.name), true),
    };
    crate::data::private_dir(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = |f: &str| dir.join(f);

    // The blueprint is the truth for units and the key, every run; pairings,
    // programs and timers persist when the server has a state-dir.
    let config = app_config(managed(bp, server), &server.api_key);
    breeze_store::save(path("config.json"), &config, Mode::AdminReadable).map_err(|e| e.to_string())?;
    preset_devices(bp, server, &path("devices.json"))?;

    let port = free_port()?;
    let settings = Settings {
        config_path: path("config.json"),
        devices_path: path("devices.json"),
        programs_path: path("programs.json"),
        timers_path: path("timers.json"),
        bind: format!("127.0.0.1:{port}"),
        min_auth_version: server.build.min_auth_version,
        worker_threads: 8,
        bg_workers: 1,
        // The front sends the client's address in X-Forwarded-For; without
        // this every client would look like 127.0.0.1, which is "the LAN", and
        // could approve its own pairing.
        behind_proxy: true,
        security_headers: true,
        timer_tick_seconds: 15,
        sched_tick_seconds: 30,
        stream_tick_seconds: 5,
        keep_warm: breeze_http::state::KeepWarm::DEFAULT,
        history_size: 720,
        code_ttl_seconds: server.pairing.code_ttl,
        token_ttl_days: server.pairing.token_ttl_days,
        auth_skew_seconds: server.pairing.skew_seconds,
    };
    let version: &'static str = Box::leak(server.build.version.clone().into_boxed_str());
    let state = Arc::new(AppState::load(settings, version).map_err(|e| format!("{e:?}"))?);
    // Exactly what `breeze-core serve` starts beside the HTTP server: without
    // them timers never fire, programs never run, and the event stream never
    // pushes a frame. Their handles are dropped; the threads run for the life
    // of the process, as they do in Breeze Core.
    let _ = breeze_http::timer_routes::spawn_runner(Arc::clone(&state));
    let _ = breeze_http::program_routes::spawn_scheduler(Arc::clone(&state));
    let _ = breeze_http::stream::spawn_poller(Arc::clone(&state));
    let _ = breeze_http::warm::spawn_keep_warm(Arc::clone(&state));
    let serving = Arc::clone(&state);
    let name = server.name.clone();
    std::thread::spawn(move || {
        if let Err(e) = breeze_http::serve(serving) {
            crate::error!(name, "the Breeze Core behind this server stopped: {e}");
        }
    });
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err() {
        if Instant::now() > deadline {
            return Err(format!("the Breeze Core behind server {:?} did not start", server.name));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(Backend {
        addr,
        state,
        dir,
        temporary,
    })
}
