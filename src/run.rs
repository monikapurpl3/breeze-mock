//! `breeze-mock run`: a blueprint, brought to life until Ctrl-C.

use std::fs::File;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::blueprint::{self, Blueprint, ProtocolVersion, SegmentMode};
use crate::lan::{device, discovery};
use crate::sim::{self, World};
use crate::{api, console, debug, info, net};

/// Where this run keeps the stores of servers without a state-dir; removed
/// when the run ends. Unique per start, so tests in one process can each run
/// a world, and stamped with the time, so a process id the system hands out
/// again never lands in a directory a dead run left behind.
fn run_dir(bp: &Blueprint) -> PathBuf {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("breeze-mock-{}-{}-{n}-{t}", bp.name, std::process::id()))
}

/// The run directory's lock, held for as long as the run lives. A run that is
/// killed outright cannot remove its directory, but the system drops its lock,
/// and that is how a later start tells an abandoned directory from a live one.
struct RunDir {
    path: PathBuf,
    lock: Mutex<Option<File>>,
}

impl RunDir {
    fn create(path: PathBuf) -> Result<RunDir, String> {
        crate::data::private_dir(&path).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        let lock = File::create(path.join(LOCK)).map_err(|e| format!("cannot create {}: {e}", path.display()))?;
        lock.try_lock()
            .map_err(|e| format!("cannot lock {}: {e}", path.join(LOCK).display()))?;
        Ok(RunDir {
            path,
            lock: Mutex::new(Some(lock)),
        })
    }

    /// Unlock first: Windows will not delete a file that is still open.
    fn remove(&self) {
        if let Ok(mut l) = self.lock.lock() {
            l.take();
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// So a start that fails halfway leaves nothing behind either.
impl Drop for RunDir {
    fn drop(&mut self) {
        self.remove();
    }
}

const LOCK: &str = ".lock";

/// Remove the run directories of runs that were killed outright: those whose
/// lock nobody holds. One whose lock file is only seconds old is left alone,
/// as a run may be between creating it and locking it.
fn sweep_abandoned() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let ours = e.file_name().to_str().is_some_and(|n| n.starts_with("breeze-mock-"));
        if !ours || !path.is_dir() {
            continue;
        }
        let lock = path.join(LOCK);
        let old = std::fs::metadata(&lock)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(10));
        // Taken and let go at once, before the removal.
        let abandoned = old && File::open(&lock).is_ok_and(|f| f.try_lock().is_ok());
        if abandoned && std::fs::remove_dir_all(&path).is_ok() {
            debug!("run", "removed {}, left by a run that was killed", path.display());
        }
    }
}

/// A world that is running: what `run` keeps alive until Ctrl-C, and what a
/// test holds for as long as it needs it. Dropping it takes the network setup
/// down and removes the run's stores; the threads end with the process.
pub struct Running {
    pub world: sim::Shared,
    pub servers: Vec<Arc<api::front::MockServer>>,
    pub guard: Arc<net::Guard>,
    dir: Arc<RunDir>,
}

impl Running {
    /// Where the run keeps the stores of servers without a state-dir.
    pub fn dir(&self) -> &Path {
        &self.dir.path
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.guard.release();
        self.dir.remove();
    }
}

/// Bring a blueprint to life: network, simulation, units, discovery, mock
/// servers and the console. Returns once everything is listening.
pub fn start(bp: &Blueprint) -> Result<Running, String> {
    let guard = Arc::new(net::setup(bp)?);
    sweep_abandoned();
    let dir = Arc::new(RunDir::create(run_dir(bp))?);

    let world: sim::Shared = Arc::new(Mutex::new(World::new(bp)));
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
        api::start(bp, &dir.path)?
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
    Ok(Running {
        world,
        servers,
        guard,
        dir,
    })
}

/// `breeze-mock run`: start, say how to reach everything, and keep going
/// until Ctrl-C or SIGTERM.
pub fn run(path: &Path) -> Result<(), String> {
    let bp = blueprint::load(path).map_err(|e| e.to_string())?;
    info!("run", "blueprint {:?}: {} unit(s), {} server(s)", bp.name, bp.units.len(), bp.servers.len());
    let running = start(&bp)?;
    {
        let guard = Arc::clone(&running.guard);
        let dir = Arc::clone(&running.dir);
        ctrlc::set_handler(move || {
            info!("run", "stopping");
            guard.release();
            dir.remove();
            std::process::exit(0);
        })
        .map_err(|e| format!("cannot catch Ctrl-C: {e}"))?;
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
        // It has no login: anyone who can reach it can change the weather,
        // take units offline and approve pairings on the mock servers.
        if !c.address.is_loopback() {
            s.push_str("    (not on loopback: anyone who can reach this address can use it)\n");
        }
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
