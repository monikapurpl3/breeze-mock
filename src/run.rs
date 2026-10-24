//! `breeze-mock run`: a blueprint, brought to life until Ctrl-C.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::blueprint::{self, Blueprint, ProtocolVersion, SegmentMode};
use crate::lan::{device, discovery};
use crate::sim::{self, World};
use crate::{api, console, info, net};

/// Where this run keeps the stores of servers without a state-dir; removed
/// when the run ends.
fn run_dir(bp: &Blueprint) -> PathBuf {
    std::env::temp_dir().join(format!("breeze-mock-{}-{}", bp.name, std::process::id()))
}

pub fn run(path: &Path) -> Result<(), String> {
    let bp = blueprint::load(path).map_err(|e| e.to_string())?;
    info!("run", "blueprint {:?}: {} unit(s), {} server(s)", bp.name, bp.units.len(), bp.servers.len());

    let guard = Arc::new(net::setup(&bp)?);
    let dir = run_dir(&bp);
    {
        let guard = Arc::clone(&guard);
        let dir = dir.clone();
        ctrlc::set_handler(move || {
            info!("run", "stopping");
            guard.release();
            let _ = std::fs::remove_dir_all(&dir);
            std::process::exit(0);
        })
        .map_err(|e| format!("cannot catch Ctrl-C: {e}"))?;
    }

    let world: sim::Shared = Arc::new(Mutex::new(World::new(&bp)));
    sim::spawn_ticker(Arc::clone(&world));

    let mut answering = Vec::new();
    for (i, u) in bp.units.iter().enumerate() {
        device::start(Arc::clone(&world), i, SocketAddr::from((u.ip, u.protocol.port)))
            .map_err(|e| format!("unit {:?}: cannot listen on {}:{}: {e}", u.key, u.ip, u.protocol.port))?;
        let seg = bp.segments.iter().find(|s| s.name == u.segment).expect("checked");
        if seg.discovery {
            answering.push(i);
        }
    }
    if !answering.is_empty() {
        discovery::start(Arc::clone(&world), answering);
    }

    let servers = if bp.servers.is_empty() {
        Vec::new()
    } else {
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        api::start(&bp, &dir)?
    };
    if let Some(c) = &bp.console {
        console::serve(
            console::Console {
                world: Arc::clone(&world),
                servers: servers.clone(),
            },
            SocketAddr::new(c.address, c.port),
        )?;
    }

    summary(&bp, path);
    loop {
        std::thread::park();
    }
}

/// What is running and how to reach it, once, at the start.
fn summary(bp: &Blueprint, path: &Path) {
    let mut s = String::from("\nready.\n");
    if !bp.units.is_empty() {
        s.push_str("  units:\n");
        for u in &bp.units {
            let seg = bp.segments.iter().find(|x| x.name == u.segment).expect("checked");
            s.push_str(&format!(
                "    {:<14} {}:{:<5} {}  id {}{}\n",
                u.identity.name,
                u.ip,
                u.protocol.port,
                if u.protocol.version == ProtocolVersion::V3 { "V3" } else { "V2" },
                u.id,
                match seg.mode {
                    SegmentMode::Loopback => "  (this machine only)",
                    _ => "",
                }
            ));
        }
    }
    for srv in &bp.servers {
        s.push_str(&format!(
            "  server {} -- Breeze Core {} ({} profile), API key {}\n",
            srv.name, srv.build.version, srv.build.profile, srv.api_key
        ));
        for l in &srv.listens {
            let scheme = if l.tls { "https" } else { "http" };
            // A port this server has to itself answers any name; a shared one
            // only the server's own domains.
            let shared = bp
                .servers
                .iter()
                .filter(|o| o.listens.iter().any(|x| x.address == l.address && x.port == l.port))
                .count()
                > 1;
            if !shared {
                s.push_str(&format!("    {scheme}://{}:{}/\n", l.address, l.port));
            }
            for d in &srv.domains {
                s.push_str(&format!("    {scheme}://{}:{}/  (resolve {} to {})\n", d.name, l.port, d.name, l.address));
            }
        }
    }
    if let Some(c) = &bp.console {
        s.push_str(&format!("  console: http://{}:{}/\n", c.address, c.port));
    }
    s.push_str(&format!(
        "  a real Breeze Core for these units: breeze-mock export breeze-core-config {} -o config.json\n",
        path.display()
    ));
    if bp.servers.iter().any(|x| x.domains.iter().any(|d| d.tls == blueprint::TlsSource::SelfSigned)) {
        s.push_str(&format!(
            "  the certificate to trust: breeze-mock export ca {} -o breeze-mock-ca.pem\n",
            path.display()
        ));
    }
    s.push_str("  Ctrl-C stops it.\n");
    eprint!("{s}");
}
