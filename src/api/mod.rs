//! Mock Breeze Core servers.
//!
//! Each `<server>` is a real Breeze Core (`backend`) behind a front that adds
//! only what a mock is for: listeners on the blueprint's addresses, TLS with a
//! certificate per domain, several servers on one port told apart by host,
//! faults and latency, the release profile (`profile`), and the pairing
//! policy.

pub mod backend;
pub mod front;
pub mod http;
pub mod profile;
pub mod tls;

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use crate::blueprint::{Blueprint, TlsSource};
use front::{Listener, MockServer};

/// A socket's TLS flag and the servers on it.
type Shared = (bool, Vec<Arc<MockServer>>);

/// Start every server in the blueprint. The run directory holds the stores
/// of servers without a state-dir.
pub fn start(bp: &Blueprint, run_dir: &Path) -> Result<Vec<Arc<MockServer>>, String> {
    let mut servers = Vec::new();
    for def in &bp.servers {
        let backend = backend::start(bp, def, run_dir)?;
        crate::info!(def.name, "Breeze Core {} ({}) behind it on {}", def.build.version, def.build.profile, backend.addr);
        servers.push(MockServer::new(def.clone(), backend, bp.seed));
    }

    // Listeners are shared between servers that name the same address and
    // port; the blueprint has already checked such servers can be told apart.
    let mut sockets: BTreeMap<(IpAddr, u16), Shared> = BTreeMap::new();
    for s in &servers {
        for l in &s.def.listens {
            let entry = sockets.entry((l.address, l.port)).or_insert((l.tls, Vec::new()));
            entry.1.push(Arc::clone(s));
        }
    }

    let needs_ca = servers
        .iter()
        .any(|s| s.def.domains.iter().any(|d| d.tls == TlsSource::SelfSigned));
    let ca = if needs_ca {
        Some(tls::Ca::load_or_create(&crate::data::for_blueprint(&bp.name), &bp.name)?)
    } else {
        None
    };

    for ((address, port), (is_tls, members)) in sockets {
        let config = if is_tls {
            let mut resolver = tls::Resolver {
                by_name: Default::default(),
                fallback: None,
            };
            for s in &members {
                let own: Vec<String> = s
                    .def
                    .domains
                    .iter()
                    .filter(|d| d.tls == TlsSource::SelfSigned)
                    .map(|d| d.name.clone())
                    .collect();
                if !own.is_empty() {
                    let ca = ca.as_ref().expect("made when any domain is self-signed");
                    let cert = ca.issue(&own, &[address])?;
                    for n in &own {
                        resolver.by_name.insert(n.to_ascii_lowercase(), Arc::clone(&cert));
                    }
                    resolver.fallback.get_or_insert(cert);
                }
                for d in &s.def.domains {
                    if let TlsSource::Files { cert, key } = &d.tls {
                        let ck = tls::from_files(cert, key)?;
                        resolver.by_name.insert(d.name.to_ascii_lowercase(), Arc::clone(&ck));
                        resolver.fallback.get_or_insert(ck);
                    }
                }
            }
            Some(tls::server_config(resolver)?)
        } else {
            None
        };
        front::serve(Listener {
            addr: SocketAddr::new(address, port),
            tls: config,
            servers: members,
        })?;
    }
    Ok(servers)
}
